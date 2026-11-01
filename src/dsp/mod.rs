//! Ланцюжок обробки голосу: гейт → висота/тембр → ефекти → гучність.

pub mod effects;
pub mod pitch;

use serde::{Deserialize, Serialize};

use effects::{Biquad, BitCrusher, Echo, NoiseGate, Reverb, RingMod};
use pitch::PitchShifter;
pub use pitch::SpectralMode;

/// Усі налаштування голосу. Саме це зберігається в пресет.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceParams {
    /// Висота голосу, півтони (-12..+12).
    pub pitch: f32,
    /// Тембр / «розмір горла», півтони (-12..+12).
    pub formant: f32,
    /// Звичайний / робот / шепіт.
    pub mode: SpectralMode,
    /// Поріг шумового гейта, дБ (-80 = вимкнено).
    pub gate_db: f32,
    /// Частота кільцевого модулятора, Гц.
    pub ring_freq: f32,
    /// Сила кільцевого модулятора (0..1).
    pub ring_mix: f32,
    /// Зрізати низи нижче цієї частоти, Гц (20 = вимкнено).
    pub low_cut: f32,
    /// Зрізати верхи вище цієї частоти, Гц (20000 = вимкнено).
    pub high_cut: f32,
    /// Хрипіння / перевантаження (0..1).
    pub drive: f32,
    /// Біти для біткрашера (16 = вимкнено).
    pub crush_bits: u32,
    /// Пониження частоти дискретизації (1 = вимкнено).
    pub crush_rate: u32,
    /// Затримка луни, мс.
    pub echo_ms: f32,
    /// Скільки разів луна повторюється (0..0.95).
    pub echo_feedback: f32,
    /// Гучність луни (0..1).
    pub echo_mix: f32,
    /// Розмір «кімнати» реверберації (0..1).
    pub reverb_size: f32,
    /// Гучність реверберації (0..1).
    pub reverb_mix: f32,
    /// Загальна гучність, дБ.
    pub volume_db: f32,
}

impl Default for VoiceParams {
    fn default() -> Self {
        Self {
            pitch: 0.0,
            formant: 0.0,
            mode: SpectralMode::Normal,
            gate_db: -80.0,
            ring_freq: 80.0,
            ring_mix: 0.0,
            low_cut: 20.0,
            high_cut: 20_000.0,
            drive: 0.0,
            crush_bits: 16,
            crush_rate: 1,
            echo_ms: 250.0,
            echo_feedback: 0.3,
            echo_mix: 0.0,
            reverb_size: 0.5,
            reverb_mix: 0.0,
            volume_db: 0.0,
        }
    }
}

pub struct VoiceChain {
    sample_rate: f32,
    params: VoiceParams,
    gate: NoiseGate,
    shifter: PitchShifter,
    ring: RingMod,
    low_cut: Biquad,
    high_cut: Biquad,
    crusher: BitCrusher,
    echo: Echo,
    reverb: Reverb,
    gain: f32,
}

impl VoiceChain {
    pub fn new(sample_rate: f32) -> Self {
        let mut chain = Self {
            sample_rate,
            params: VoiceParams::default(),
            gate: NoiseGate::new(sample_rate),
            shifter: PitchShifter::new(sample_rate),
            ring: RingMod::new(sample_rate),
            low_cut: Biquad::default(),
            high_cut: Biquad::default(),
            crusher: BitCrusher::default(),
            echo: Echo::new(sample_rate),
            reverb: Reverb::new(sample_rate),
            gain: 1.0,
        };
        chain.set_params(&VoiceParams::default());
        chain
    }

    pub fn set_params(&mut self, p: &VoiceParams) {
        self.params = *p;
        self.shifter.set_params(p.pitch, p.formant, p.mode);
        self.low_cut.set_highpass(self.sample_rate, p.low_cut);
        self.high_cut.set_lowpass(self.sample_rate, p.high_cut);
    }

    pub fn process(&mut self, x: f32) -> f32 {
        let p = &self.params;
        let mut y = self.gate.process(x, p.gate_db);
        y = self.shifter.process(y);
        y = self.ring.process(y, p.ring_freq, p.ring_mix);
        y = self.low_cut.process(y);
        y = self.high_cut.process(y);
        y = effects::drive(y, p.drive);
        y = self.crusher.process(y, p.crush_bits, p.crush_rate);
        y = self.echo.process(y, p.echo_ms, p.echo_feedback, p.echo_mix);
        y = self.reverb.process(y, p.reverb_size, p.reverb_mix);

        // Гучність змінюємо плавно, інакше чути клацання.
        let target = effects::db_to_gain(p.volume_db);
        self.gain += (target - self.gain) * 0.001;
        (y * self.gain).clamp(-1.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preset;

    #[test]
    fn all_builtin_presets_are_stable() {
        let sr = 48_000.0;
        for preset in preset::builtin() {
            let mut chain = VoiceChain::new(sr);
            chain.set_params(&preset.params);
            let mut energy = 0.0;
            for i in 0..48_000 {
                // Імітація голосу: 150 Гц з гармоніками.
                let t = i as f32 / sr;
                let x = (0..8)
                    .map(|h| {
                        (2.0 * std::f32::consts::PI * 150.0 * (h + 1) as f32 * t).sin()
                            / (h + 1) as f32
                    })
                    .sum::<f32>()
                    * 0.2;
                let y = chain.process(x);
                assert!(y.is_finite(), "{}: NaN", preset.name);
                if i > 24_000 {
                    energy += y * y;
                }
            }
            assert!(energy > 1.0, "{}: майже тиша ({energy})", preset.name);
        }
    }
}

#[cfg(test)]
mod loudness_probe {
    use super::*;
    #[test]
    #[ignore]
    fn print_preset_loudness() {
        let sr = 48_000.0;
        for preset in crate::preset::builtin() {
            let mut chain = VoiceChain::new(sr);
            chain.set_params(&preset.params);
            let (mut ein, mut eout, mut peak) = (0.0f64, 0.0f64, 0.0f32);
            for i in 0..96_000 {
                let t = i as f32 / sr;
                // "голос": 250 Гц з гармоніками, з амплітудною модуляцією як у мові
                let am = 0.5 + 0.5 * (2.0 * std::f32::consts::PI * 3.0 * t).sin().abs();
                let x = (0..12)
                    .map(|h| {
                        (2.0 * std::f32::consts::PI * 250.0 * (h + 1) as f32 * t).sin()
                            / (h + 1) as f32
                    })
                    .sum::<f32>()
                    * 0.15
                    * am;
                let y = chain.process(x);
                if i > 24_000 {
                    ein += (x * x) as f64;
                    eout += (y * y) as f64;
                    peak = peak.max(y.abs());
                }
            }
            println!(
                "{:20} gain {:+5.1} dB  peak {:.2}",
                preset.name,
                10.0 * (eout / ein).log10(),
                peak
            );
        }
    }
}
