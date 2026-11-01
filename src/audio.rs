//! Звук: мікрофон → обробка → віртуальний кабель (+ навушники, щоб чути себе).
//!
//! Мікрофон і вихід мають різні «годинники», тому між ними стоїть кільцевий
//! буфер, а вихід читає з нього з ледь-ледь змінною швидкістю (ресемплер),
//! щоб буфер не переповнювався і не пустів.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use crate::dsp::{VoiceChain, VoiceParams};

/// Початковий запас у буфері між мікрофоном і виходом.
const START_BUFFER_MS: f32 = 25.0;
/// Якщо звук рветься, запас автоматично збільшується, але не більше цього.
const MAX_BUFFER_MS: f32 = 90.0;

/// Те, що спільне між інтерфейсом і аудіопотоками.
pub struct Shared {
    params: Mutex<VoiceParams>,
    generation: AtomicU64,
    pub effect_on: AtomicBool,
    in_level: AtomicU32,
    out_level: AtomicU32,
    buffer_ms: AtomicU32,
    error: Mutex<Option<String>>,
}

impl Shared {
    pub fn new(params: VoiceParams) -> Self {
        Self {
            params: Mutex::new(params),
            generation: AtomicU64::new(1),
            effect_on: AtomicBool::new(true),
            in_level: AtomicU32::new(0),
            out_level: AtomicU32::new(0),
            buffer_ms: AtomicU32::new(START_BUFFER_MS.to_bits()),
            error: Mutex::new(None),
        }
    }

    pub fn set_params(&self, p: VoiceParams) {
        if let Ok(mut guard) = self.params.lock() {
            *guard = p;
        }
        self.generation.fetch_add(1, Ordering::Release);
    }

    pub fn levels(&self) -> (f32, f32) {
        (
            f32::from_bits(self.in_level.load(Ordering::Relaxed)),
            f32::from_bits(self.out_level.load(Ordering::Relaxed)),
        )
    }

    /// Поточний запас буфера (мс) — це частина затримки.
    pub fn buffer_ms(&self) -> f32 {
        f32::from_bits(self.buffer_ms.load(Ordering::Relaxed))
    }

    pub fn take_error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|mut e| e.take())
    }

    fn set_error(&self, msg: String) {
        if let Ok(mut e) = self.error.lock() {
            *e = Some(msg);
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
}

fn device_name(d: &Device) -> String {
    d.description()
        .map(|desc| desc.name().to_string())
        .unwrap_or_else(|_| "Невідомий пристрій".into())
}

fn device_info(d: &Device) -> Option<DeviceInfo> {
    Some(DeviceInfo {
        id: d.id().ok()?.to_string(),
        name: device_name(d),
    })
}

pub fn list_inputs() -> Vec<DeviceInfo> {
    let host = cpal::default_host();
    host.input_devices()
        .map(|it| it.filter_map(|d| device_info(&d)).collect())
        .unwrap_or_default()
}

pub fn list_outputs() -> Vec<DeviceInfo> {
    let host = cpal::default_host();
    host.output_devices()
        .map(|it| it.filter_map(|d| device_info(&d)).collect())
        .unwrap_or_default()
}

pub fn default_output() -> Option<DeviceInfo> {
    cpal::default_host()
        .default_output_device()
        .and_then(|d| device_info(&d))
}

/// Шукаємо VB-Audio Virtual Cable (його вхід — це наш вихід).
pub fn find_virtual_cable(outputs: &[DeviceInfo]) -> Option<DeviceInfo> {
    outputs
        .iter()
        .find(|d| d.name.to_lowercase().contains("cable input"))
        .or_else(|| {
            outputs
                .iter()
                .find(|d| d.name.to_lowercase().contains("cable"))
        })
        .cloned()
}

fn find_device(id: &str) -> Option<Device> {
    let host = cpal::default_host();
    host.device_by_id(&id.parse().ok()?)
}

pub struct EngineConfig {
    pub input: Option<String>,
    pub output: Option<String>,
    pub monitor: Option<String>,
}

/// Працюючий звук. Поки ця структура жива — звук іде; `drop` зупиняє все.
pub struct Engine {
    _streams: Vec<Stream>,
    pub summary: String,
}

pub fn start(cfg: &EngineConfig, shared: Arc<Shared>) -> Result<Engine, String> {
    let host = cpal::default_host();

    let input = match &cfg.input {
        Some(id) => find_device(id).ok_or("Мікрофон не знайдено — обери інший")?,
        None => host
            .default_input_device()
            .ok_or("У системі немає мікрофона")?,
    };
    let in_cfg = input
        .default_input_config()
        .map_err(|e| format!("Мікрофон не відповідає: {e}"))?;
    let in_rate = in_cfg.sample_rate() as f32;

    let mut targets: Vec<Device> = Vec::new();
    if let Some(id) = &cfg.output {
        targets.push(find_device(id).ok_or("Вихідний пристрій не знайдено — обери інший")?);
    }
    if let Some(id) = &cfg.monitor
        && cfg.output.as_deref() != Some(id.as_str())
    {
        targets.push(find_device(id).ok_or("Навушники не знайдено — обери інші")?);
    }
    if targets.is_empty() {
        return Err("Обери, куди виводити звук".into());
    }

    shared
        .buffer_ms
        .store(START_BUFFER_MS.to_bits(), Ordering::Relaxed);

    let mut streams = Vec::new();
    let mut producers = Vec::new();
    let mut out_names = Vec::new();
    for dev in &targets {
        let rb = HeapRb::<f32>::new((in_rate * 0.5) as usize);
        let (prod, cons) = rb.split();
        producers.push(prod);
        let out_cfg = dev
            .default_output_config()
            .map_err(|e| format!("{}: {e}", device_name(dev)))?;
        let reader = Resampler::new(cons, in_rate, out_cfg.sample_rate() as f32, shared.clone());
        let stream = build_output(
            dev,
            out_cfg.sample_format(),
            out_cfg.config(),
            reader,
            shared.clone(),
        )?;
        streams.push(stream);
        out_names.push(format!(
            "{} ({} Гц)",
            device_name(dev),
            out_cfg.sample_rate()
        ));
    }

    let stream = build_input(
        &input,
        in_cfg.sample_format(),
        in_cfg.config(),
        producers,
        shared.clone(),
    )?;
    streams.insert(0, stream);

    for s in &streams {
        s.play()
            .map_err(|e| format!("Не вдалося запустити звук: {e}"))?;
    }

    Ok(Engine {
        _streams: streams,
        summary: format!(
            "🎤 {} ({} Гц) → {}",
            device_name(&input),
            in_cfg.sample_rate(),
            out_names.join(" + ")
        ),
    })
}

fn report_error(shared: &Arc<Shared>) -> impl FnMut(cpal::Error) + Send + 'static {
    let shared = shared.clone();
    move |e| shared.set_error(format!("Помилка звуку: {e}"))
}

// ---------------------------------------------------------------------------
// Вхід
// ---------------------------------------------------------------------------

fn build_input(
    dev: &Device,
    format: SampleFormat,
    config: StreamConfig,
    producers: Vec<HeapProd<f32>>,
    shared: Arc<Shared>,
) -> Result<Stream, String> {
    macro_rules! go {
        ($t:ty) => {
            build_input_typed::<$t>(dev, config, producers, shared)
        };
    }
    match format {
        SampleFormat::F32 => go!(f32),
        SampleFormat::I16 => go!(i16),
        SampleFormat::I32 => go!(i32),
        SampleFormat::U16 => go!(u16),
        SampleFormat::U8 => go!(u8),
        SampleFormat::I8 => go!(i8),
        SampleFormat::F64 => go!(f64),
        other => Err(format!("Формат мікрофона {other} не підтримується")),
    }
}

fn build_input_typed<T>(
    dev: &Device,
    config: StreamConfig,
    mut producers: Vec<HeapProd<f32>>,
    shared: Arc<Shared>,
) -> Result<Stream, String>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = config.channels.max(1) as usize;
    let mut chain = VoiceChain::new(config.sample_rate as f32);
    let mut seen_generation = 0u64;
    let err = report_error(&shared);
    let sh = shared.clone();

    let callback = move |data: &[T], _: &cpal::InputCallbackInfo| {
        // Нові налаштування з інтерфейсу. try_lock: аудіопотік ніколи не чекає.
        let generation = sh.generation.load(Ordering::Acquire);
        if generation != seen_generation
            && let Ok(p) = sh.params.try_lock()
        {
            chain.set_params(&p);
            seen_generation = generation;
        }
        let effect_on = sh.effect_on.load(Ordering::Relaxed);

        let (mut peak_in, mut peak_out) = (0.0f32, 0.0f32);
        for frame in data.chunks(channels) {
            let x = frame.iter().map(|s| f32::from_sample(*s)).sum::<f32>() / channels as f32;
            // Обробляємо завжди, щоб перемикач спрацьовував миттєво.
            let processed = chain.process(x);
            let y = if effect_on { processed } else { x };
            peak_in = peak_in.max(x.abs());
            peak_out = peak_out.max(y.abs());
            for p in producers.iter_mut() {
                let _ = p.try_push(y);
            }
        }
        sh.in_level.store(peak_in.to_bits(), Ordering::Relaxed);
        sh.out_level.store(peak_out.to_bits(), Ordering::Relaxed);
    };

    dev.build_input_stream::<T, _, _>(config, callback, err, None)
        .map_err(|e| format!("Не вдалося відкрити мікрофон: {e}"))
}

// ---------------------------------------------------------------------------
// Вихід
// ---------------------------------------------------------------------------

/// Читає семпли з буфера і підлаштовує швидкість читання під заповненість буфера.
struct Resampler {
    cons: HeapCons<f32>,
    in_rate: f32,
    step: f64,
    pos: f64,
    prev: f32,
    cur: f32,
    target: usize,
    fill_avg: f64,
    primed: bool,
    shared: Arc<Shared>,
}

impl Resampler {
    fn new(cons: HeapCons<f32>, in_rate: f32, out_rate: f32, shared: Arc<Shared>) -> Self {
        let target = ms_to_samples(START_BUFFER_MS, in_rate);
        Self {
            cons,
            in_rate,
            step: in_rate as f64 / out_rate as f64,
            pos: 0.0,
            prev: 0.0,
            cur: 0.0,
            target,
            fill_avg: target as f64,
            primed: false,
            shared,
        }
    }

    fn fill<T: Sample + FromSample<f32>>(&mut self, data: &mut [T], channels: usize) {
        let avail = self.cons.occupied_len();
        if !self.primed {
            if avail < self.target {
                data.fill(T::EQUILIBRIUM);
                return;
            }
            self.primed = true;
            self.fill_avg = avail as f64;
        }

        // Дуже відстали (наприклад, комп «підвис») — просто наздоганяємо.
        if avail > self.target * 4 {
            self.cons.skip(avail - self.target);
            self.fill_avg = self.target as f64;
        }

        self.fill_avg += (self.cons.occupied_len() as f64 - self.fill_avg) * 0.05;
        let error = (self.fill_avg - self.target as f64) / self.target as f64;
        let step = self.step * (1.0 + (error * 0.01).clamp(-0.02, 0.02));

        for frame in data.chunks_mut(channels) {
            while self.pos >= 1.0 {
                self.prev = self.cur;
                match self.cons.try_pop() {
                    Some(s) => self.cur = s,
                    None => {
                        self.on_underrun();
                        self.cur = 0.0;
                    }
                }
                self.pos -= 1.0;
            }
            let y = self.prev + (self.cur - self.prev) * self.pos as f32;
            self.pos += step;
            let s = T::from_sample(y);
            frame.iter_mut().for_each(|c| *c = s);
        }
    }

    /// Буфер спорожнів: звук перервався. Збільшуємо запас і чекаємо, поки набереться.
    fn on_underrun(&mut self) {
        if !self.primed {
            return;
        }
        self.primed = false;
        let max = ms_to_samples(MAX_BUFFER_MS, self.in_rate);
        self.target = (self.target + ms_to_samples(5.0, self.in_rate)).min(max);
        let ms = self.target as f32 * 1000.0 / self.in_rate;
        self.shared.buffer_ms.store(ms.to_bits(), Ordering::Relaxed);
    }
}

fn ms_to_samples(ms: f32, rate: f32) -> usize {
    (ms / 1000.0 * rate) as usize
}

fn build_output(
    dev: &Device,
    format: SampleFormat,
    config: StreamConfig,
    reader: Resampler,
    shared: Arc<Shared>,
) -> Result<Stream, String> {
    macro_rules! go {
        ($t:ty) => {
            build_output_typed::<$t>(dev, config, reader, shared)
        };
    }
    match format {
        SampleFormat::F32 => go!(f32),
        SampleFormat::I16 => go!(i16),
        SampleFormat::I32 => go!(i32),
        SampleFormat::U16 => go!(u16),
        SampleFormat::U8 => go!(u8),
        SampleFormat::I8 => go!(i8),
        SampleFormat::F64 => go!(f64),
        other => Err(format!("Формат виходу {other} не підтримується")),
    }
}

fn build_output_typed<T>(
    dev: &Device,
    config: StreamConfig,
    mut reader: Resampler,
    shared: Arc<Shared>,
) -> Result<Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels.max(1) as usize;
    let err = report_error(&shared);
    let callback = move |data: &mut [T], _: &cpal::OutputCallbackInfo| reader.fill(data, channels);
    dev.build_output_stream::<T, _, _>(config, callback, err, None)
        .map_err(|e| format!("Не вдалося відкрити «{}»: {e}", device_name(dev)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(in_rate: f32, out_rate: f32) -> (HeapProd<f32>, Resampler) {
        let (prod, cons) = HeapRb::<f32>::new(48_000).split();
        let shared = Arc::new(Shared::new(VoiceParams::default()));
        (prod, Resampler::new(cons, in_rate, out_rate, shared))
    }

    /// Симулюємо 20 секунд: мікрофон трохи швидший за вихід (як у реальному житті).
    /// Буфер не має ні переповнитись, ні спорожніти.
    #[test]
    fn resampler_tracks_clock_drift() {
        let (mut prod, mut r) = make(48_000.0, 48_000.0);
        let mut out = vec![0.0f32; 480 * 2];
        let mut produced = 0.0f64;
        for block in 0..2000 {
            // Вхід приходить блоками по 10 мс, але на 0.5 % швидше.
            produced += 480.0 * 1.005;
            while produced >= 1.0 {
                let _ = prod.try_push(0.1);
                produced -= 1.0;
            }
            r.fill(&mut out, 2);
            if block > 300 {
                let fill = r.cons.occupied_len();
                assert!(fill < r.target * 3, "буфер росте: {fill}");
                assert!(r.primed, "звук перервався на блоці {block}");
            }
        }
    }

    #[test]
    fn resampler_converts_rates() {
        let (mut prod, mut r) = make(44_100.0, 48_000.0);
        let mut out = vec![0.0f32; 480];
        let mut produced = 0.0f64;
        for _ in 0..1000 {
            produced += 441.0;
            while produced >= 1.0 {
                let _ = prod.try_push(0.25);
                produced -= 1.0;
            }
            r.fill(&mut out, 1);
        }
        assert!(r.primed);
        assert!((out[100] - 0.25).abs() < 1e-3);
    }
}
