//! Зсув висоти голосу (pitch) і тембру (формант) у частотній області.
//!
//! Як це працює, коротко:
//! 1. Нарізаємо звук на шматочки по 1024 семпли з перекриттям 75 %.
//! 2. Кожен шматочок через FFT перетворюємо на спектр: які частоти й наскільки гучні.
//! 3. Зі спектра виділяємо «огинаючу» (кепстр) — це і є форманти, тобто тембр.
//! 4. Гармоніки голосу (висоту) зсуваємо на коефіцієнт `pitch`,
//!    а огинаючу — окремо на коефіцієнт `formant`.
//! 5. Збираємо спектр назад у звук (обернений FFT) і склеюємо шматочки.
//!
//! Алгоритм — класичний phase vocoder (S. Bernsee, smbPitchShift) плюс
//! кепстральна корекція формант.

use std::f32::consts::PI;
use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use serde::{Deserialize, Serialize};

/// Розмір вікна FFT. 1024 при 48 кГц ≈ 21 мс.
pub const FFT_SIZE: usize = 1024;
/// Скільки разів вікна перекриваються.
pub const OVERSAMPLE: usize = 4;
/// Крок між вікнами.
pub const HOP: usize = FFT_SIZE / OVERSAMPLE;
/// Затримка, яку додає цей блок (у семплах).
pub const LATENCY: usize = FFT_SIZE - HOP;
const BINS: usize = FFT_SIZE / 2 + 1;
const TWO_PI: f32 = 2.0 * PI;

/// Режим синтезу фази.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpectralMode {
    /// Звичайний голос.
    #[default]
    Normal,
    /// Робот: фаза обнуляється, голос стає монотонним «дзижчанням».
    Robot,
    /// Шепіт: фаза випадкова, зникає висота тону.
    Whisper,
}

pub struct PitchShifter {
    fwd: Arc<dyn RealToComplex<f32>>,
    inv: Arc<dyn ComplexToReal<f32>>,
    window: Vec<f32>,
    out_scale: f32,
    freq_per_bin: f32,
    lifter: usize,

    in_fifo: Vec<f32>,
    out_fifo: Vec<f32>,
    out_accum: Vec<f32>,
    rover: usize,

    time_buf: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    cep_spec: Vec<Complex<f32>>,
    scratch_fwd: Vec<Complex<f32>>,
    scratch_inv: Vec<Complex<f32>>,

    last_phase: Vec<f32>,
    sum_phase: Vec<f32>,
    ana_mag: Vec<f32>,
    ana_freq: Vec<f32>,
    syn_mag: Vec<f32>,
    syn_freq: Vec<f32>,
    env: Vec<f32>,

    rng: u32,
    /// Плавний перехід між «сухим» і обробленим сигналом, щоб не клацало.
    wet: f32,
    /// Автовирівнювання гучності: середня потужність входу й виходу.
    pow_in: f32,
    pow_out: f32,
    level_gain: f32,
    level_coef: f32,

    pitch_ratio: f32,
    formant_ratio: f32,
    mode: SpectralMode,
}

impl PitchShifter {
    pub fn new(sample_rate: f32) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fwd = planner.plan_fft_forward(FFT_SIZE);
        let inv = planner.plan_fft_inverse(FFT_SIZE);

        // Періодичне вікно Ханна.
        let window: Vec<f32> = (0..FFT_SIZE)
            .map(|k| 0.5 - 0.5 * (TWO_PI * k as f32 / FFT_SIZE as f32).cos())
            .collect();
        // Сума квадратів вікна на крок — нормування після overlap-add.
        let w2: f32 = window.iter().map(|w| w * w).sum::<f32>() / HOP as f32;
        let out_scale = 1.0 / (FFT_SIZE as f32 * w2);

        // Ліфтер ≈ 1 мс: огинаюча без окремих гармонік навіть для високого дитячого голосу.
        let lifter = ((sample_rate / 1000.0) as usize).clamp(8, FFT_SIZE / 8);

        Self {
            scratch_fwd: fwd.make_scratch_vec(),
            scratch_inv: inv.make_scratch_vec(),
            spectrum: fwd.make_output_vec(),
            cep_spec: fwd.make_output_vec(),
            fwd,
            inv,
            window,
            out_scale,
            freq_per_bin: sample_rate / FFT_SIZE as f32,
            lifter,
            in_fifo: vec![0.0; FFT_SIZE],
            out_fifo: vec![0.0; FFT_SIZE],
            out_accum: vec![0.0; FFT_SIZE],
            rover: LATENCY,
            time_buf: vec![0.0; FFT_SIZE],
            last_phase: vec![0.0; BINS],
            sum_phase: vec![0.0; BINS],
            ana_mag: vec![0.0; BINS],
            ana_freq: vec![0.0; BINS],
            syn_mag: vec![0.0; BINS],
            syn_freq: vec![0.0; BINS],
            env: vec![1.0; BINS],
            rng: 0x1234_5678,
            wet: 0.0,
            pow_in: 0.0,
            pow_out: 0.0,
            level_gain: 1.0,
            level_coef: 1.0 - (-1.0 / (0.3 * sample_rate)).exp(),
            pitch_ratio: 1.0,
            formant_ratio: 1.0,
            mode: SpectralMode::Normal,
        }
    }

    /// `pitch` і `formant` — у півтонах (+12 = на октаву вище).
    pub fn set_params(&mut self, pitch_semitones: f32, formant_semitones: f32, mode: SpectralMode) {
        self.pitch_ratio = semitones_to_ratio(pitch_semitones);
        self.formant_ratio = semitones_to_ratio(formant_semitones);
        self.mode = mode;
    }

    fn is_identity(&self) -> bool {
        self.mode == SpectralMode::Normal
            && (self.pitch_ratio - 1.0).abs() < 1e-4
            && (self.formant_ratio - 1.0).abs() < 1e-4
    }

    /// Обробляє один семпл. Вихід затриманий на [`LATENCY`] семплів.
    pub fn process(&mut self, input: f32) -> f32 {
        self.in_fifo[self.rover] = input;
        let dry = self.in_fifo[self.rover - LATENCY];
        let wet = self.out_fifo[self.rover - LATENCY];
        self.rover += 1;
        if self.rover >= FFT_SIZE {
            self.rover = LATENCY;
            self.process_frame();
        }

        // Повільно підганяємо гучність обробленого звуку під вхідну:
        // фазовий вокодер на різних налаштуваннях губить різну кількість енергії.
        self.pow_in += (dry * dry - self.pow_in) * self.level_coef;
        self.pow_out += (wet * wet - self.pow_out) * self.level_coef;
        let target_gain = ((self.pow_in + 1e-7) / (self.pow_out + 1e-7))
            .sqrt()
            .clamp(0.5, 4.0);
        self.level_gain += (target_gain - self.level_gain) * self.level_coef;
        let wet = wet * self.level_gain;

        // Якщо ефект нульовий — віддаємо чистий звук, але перемикаємось плавно.
        let target = if self.is_identity() { 0.0 } else { 1.0 };
        self.wet += (target - self.wet) * 0.002;
        dry + (wet - dry) * self.wet
    }

    fn process_frame(&mut self) {
        let expct = TWO_PI * HOP as f32 / FFT_SIZE as f32;
        let osamp = OVERSAMPLE as f32;

        // ---- Аналіз ----
        for k in 0..FFT_SIZE {
            self.time_buf[k] = self.in_fifo[k] * self.window[k];
        }
        let _ = self.fwd.process_with_scratch(
            &mut self.time_buf,
            &mut self.spectrum,
            &mut self.scratch_fwd,
        );

        for k in 0..BINS {
            let c = self.spectrum[k];
            let mag = c.norm();
            let phase = c.arg();
            let mut delta = phase - self.last_phase[k];
            self.last_phase[k] = phase;
            delta -= k as f32 * expct;
            delta = wrap_phase(delta);
            let deviation = osamp * delta / TWO_PI;
            self.ana_freq[k] = (k as f32 + deviation) * self.freq_per_bin;
            self.ana_mag[k] = mag;
        }

        // ---- Огинаюча спектра (форманти) через кепстр ----
        self.compute_envelope();

        let e_in: f32 = self.ana_mag.iter().map(|m| m * m).sum();

        // «Вирівняний» спектр: гармоніки без тембру.
        for k in 0..BINS {
            self.ana_mag[k] /= self.env[k];
        }

        // ---- Зсув гармонік (висоти) ----
        // Кожен бин k переносимо в позицію k·pitch разом з його частотою.
        self.syn_mag.fill(0.0);
        self.syn_freq.fill(0.0);
        for k in 0..BINS {
            let idx = (k as f32 * self.pitch_ratio).round() as usize;
            if idx < BINS {
                self.syn_mag[idx] += self.ana_mag[k];
                self.syn_freq[idx] = self.ana_freq[k] * self.pitch_ratio;
            }
        }

        // ---- Накладаємо зсунутий тембр ----
        for k in 0..BINS {
            let src = k as f32 / self.formant_ratio;
            self.syn_mag[k] *= interp(&self.env, src);
        }

        // ---- Зберігаємо гучність ----
        // Без цього голос униз стає гучнішим (гармоніки злипаються),
        // а вгору — тихішим (між гармоніками з'являються дірки).
        let e_out: f32 = self.syn_mag.iter().map(|m| m * m).sum();
        if e_out > 1e-12 {
            let g = (e_in / e_out).sqrt().clamp(0.1, 10.0);
            self.syn_mag.iter_mut().for_each(|m| *m *= g);
        }

        // ---- Синтез фази ----
        for k in 0..BINS {
            let phase = match self.mode {
                SpectralMode::Normal => {
                    let mut tmp = self.syn_freq[k] - k as f32 * self.freq_per_bin;
                    tmp /= self.freq_per_bin;
                    tmp = TWO_PI * tmp / osamp;
                    tmp += k as f32 * expct;
                    self.sum_phase[k] = wrap_phase(self.sum_phase[k] + tmp);
                    self.sum_phase[k]
                }
                // Лінійна фаза πk ставить імпульс у центр вікна, а не на край,
                // інакше вікно його майже повністю «з'їдає».
                SpectralMode::Robot => PI * k as f32,
                SpectralMode::Whisper => self.next_random() * TWO_PI,
            };
            self.spectrum[k] = Complex::from_polar(self.syn_mag[k], phase);
        }
        self.spectrum[0].im = 0.0;
        self.spectrum[BINS - 1].im = 0.0;

        let _ = self.inv.process_with_scratch(
            &mut self.spectrum,
            &mut self.time_buf,
            &mut self.scratch_inv,
        );

        // ---- Overlap-add ----
        for k in 0..FFT_SIZE {
            self.out_accum[k] += self.window[k] * self.time_buf[k] * self.out_scale;
        }
        self.out_fifo[..HOP].copy_from_slice(&self.out_accum[..HOP]);
        self.out_accum.copy_within(HOP.., 0);
        self.out_accum[FFT_SIZE - HOP..].fill(0.0);
        self.in_fifo.copy_within(HOP.., 0);
    }

    fn compute_envelope(&mut self) {
        // log|X| → обернений FFT → кепстр.
        for k in 0..BINS {
            self.cep_spec[k] = Complex::new((self.ana_mag[k] + 1e-9).ln(), 0.0);
        }
        let _ = self.inv.process_with_scratch(
            &mut self.cep_spec,
            &mut self.time_buf,
            &mut self.scratch_inv,
        );

        // Залишаємо лише «повільні» складові — це огинаюча.
        let n = FFT_SIZE;
        let norm = 1.0 / n as f32;
        for i in 0..n {
            let keep = i < self.lifter || i > n - self.lifter;
            self.time_buf[i] = if keep { self.time_buf[i] * norm } else { 0.0 };
        }

        let _ = self.fwd.process_with_scratch(
            &mut self.time_buf,
            &mut self.cep_spec,
            &mut self.scratch_fwd,
        );
        for k in 0..BINS {
            self.env[k] = self.cep_spec[k].re.exp().max(1e-6);
        }
    }

    fn next_random(&mut self) -> f32 {
        // xorshift32 — швидко і без алокацій.
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        (x >> 8) as f32 / (1u32 << 24) as f32
    }
}

pub fn semitones_to_ratio(semitones: f32) -> f32 {
    2f32.powf(semitones / 12.0)
}

fn wrap_phase(p: f32) -> f32 {
    p - TWO_PI * (p / TWO_PI).round()
}

fn interp(v: &[f32], pos: f32) -> f32 {
    if pos <= 0.0 {
        return v[0];
    }
    let i = pos as usize;
    if i + 1 >= v.len() {
        return v[v.len() - 1];
    }
    let f = pos - i as f32;
    v[i] + (v[i + 1] - v[i]) * f
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    fn sine(freq: f32, len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| 0.5 * (TWO_PI * freq * i as f32 / SR).sin())
            .collect()
    }

    /// Домінантна частота через пошук піку DFT на сітці 1 Гц.
    fn dominant_freq(sig: &[f32]) -> f32 {
        let mut best = (0.0, 0.0);
        for f in (50..2000).map(|f| f as f32) {
            let (mut re, mut im) = (0.0f32, 0.0f32);
            for (i, s) in sig.iter().enumerate() {
                let ph = TWO_PI * f * i as f32 / SR;
                re += s * ph.cos();
                im += s * ph.sin();
            }
            let m = re * re + im * im;
            if m > best.1 {
                best = (f, m);
            }
        }
        best.0
    }

    fn run(shifter: &mut PitchShifter, input: &[f32]) -> Vec<f32> {
        input.iter().map(|&x| shifter.process(x)).collect()
    }

    #[test]
    fn identity_passes_signal_with_latency() {
        let mut ps = PitchShifter::new(SR);
        let input = sine(220.0, 8000);
        let out = run(&mut ps, &input);
        for i in LATENCY..input.len() {
            assert!((out[i] - input[i - LATENCY]).abs() < 1e-6);
        }
    }

    #[test]
    fn octave_up_doubles_frequency() {
        let mut ps = PitchShifter::new(SR);
        ps.set_params(12.0, 0.0, SpectralMode::Normal);
        let out = run(&mut ps, &sine(220.0, 24_000));
        let f = dominant_freq(&out[12_000..20_000]);
        assert!((f - 440.0).abs() < 8.0, "got {f}");
    }

    #[test]
    fn down_shift_lowers_frequency() {
        let mut ps = PitchShifter::new(SR);
        ps.set_params(-7.0, 0.0, SpectralMode::Normal);
        let out = run(&mut ps, &sine(330.0, 24_000));
        let f = dominant_freq(&out[12_000..20_000]);
        let expected = 330.0 * semitones_to_ratio(-7.0);
        assert!((f - expected).abs() < 8.0, "got {f}, expected {expected}");
    }

    #[test]
    fn output_level_stays_sane() {
        for mode in [
            SpectralMode::Normal,
            SpectralMode::Robot,
            SpectralMode::Whisper,
        ] {
            let mut ps = PitchShifter::new(SR);
            ps.set_params(-5.0, -3.0, mode);
            let input = sine(200.0, 24_000);
            let out = run(&mut ps, &input);
            let rms = |s: &[f32]| (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt();
            let ratio = rms(&out[12_000..]) / rms(&input[12_000..]);
            assert!(out.iter().all(|x| x.is_finite()));
            assert!((0.1..4.0).contains(&ratio), "{mode:?}: level ratio {ratio}");
        }
    }
}
