//! Прості ефекти, які працюють семпл за семплом. Кожен — маленька
//! самостійна структура, тож їх легко читати й міняти.

use std::f32::consts::PI;

/// Коефіцієнт згладжування для заданого часу (секунди).
fn smoothing_coef(seconds: f32, sample_rate: f32) -> f32 {
    1.0 - (-1.0 / (seconds * sample_rate)).exp()
}

pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

// ---------------------------------------------------------------------------
// Шумовий гейт: глушить мікрофон, коли ти мовчиш (клавіатура, вентилятор).
// ---------------------------------------------------------------------------
pub struct NoiseGate {
    env: f32,
    gain: f32,
    env_release: f32,
    open_coef: f32,
    close_coef: f32,
}

impl NoiseGate {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            env: 0.0,
            gain: 1.0,
            env_release: 1.0 - smoothing_coef(0.15, sample_rate),
            open_coef: smoothing_coef(0.002, sample_rate),
            close_coef: smoothing_coef(0.08, sample_rate),
        }
    }

    pub fn process(&mut self, x: f32, threshold_db: f32) -> f32 {
        if threshold_db <= -79.0 {
            self.gain = 1.0;
            return x;
        }
        let level = x.abs();
        self.env = if level > self.env {
            level
        } else {
            self.env * self.env_release
        };
        let open = self.env > db_to_gain(threshold_db);
        let (target, coef) = if open {
            (1.0, self.open_coef)
        } else {
            (0.0, self.close_coef)
        };
        self.gain += (target - self.gain) * coef;
        x * self.gain
    }
}

// ---------------------------------------------------------------------------
// Біквадратний фільтр (RBJ Audio EQ Cookbook).
// ---------------------------------------------------------------------------
#[derive(Default)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
    bypass: bool,
}

impl Biquad {
    pub fn set_lowpass(&mut self, sample_rate: f32, freq: f32) {
        self.bypass = freq >= sample_rate * 0.45 || freq >= 19_000.0;
        if self.bypass {
            return;
        }
        let (cos, alpha) = Self::prep(sample_rate, freq);
        let a0 = 1.0 + alpha;
        self.b0 = (1.0 - cos) / 2.0 / a0;
        self.b1 = (1.0 - cos) / a0;
        self.b2 = self.b0;
        self.a1 = -2.0 * cos / a0;
        self.a2 = (1.0 - alpha) / a0;
    }

    pub fn set_highpass(&mut self, sample_rate: f32, freq: f32) {
        self.bypass = freq <= 21.0;
        if self.bypass {
            return;
        }
        let (cos, alpha) = Self::prep(sample_rate, freq);
        let a0 = 1.0 + alpha;
        self.b0 = (1.0 + cos) / 2.0 / a0;
        self.b1 = -(1.0 + cos) / a0;
        self.b2 = self.b0;
        self.a1 = -2.0 * cos / a0;
        self.a2 = (1.0 - alpha) / a0;
    }

    fn prep(sample_rate: f32, freq: f32) -> (f32, f32) {
        let w0 = 2.0 * PI * freq / sample_rate;
        let q = std::f32::consts::FRAC_1_SQRT_2;
        (w0.cos(), w0.sin() / (2.0 * q))
    }

    pub fn process(&mut self, x: f32) -> f32 {
        if self.bypass {
            return x;
        }
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

// ---------------------------------------------------------------------------
// Кільцевий модулятор: множимо голос на синусоїду → металевий «робот».
// ---------------------------------------------------------------------------
pub struct RingMod {
    phase: f32,
    sample_rate: f32,
}

impl RingMod {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            phase: 0.0,
            sample_rate,
        }
    }

    pub fn process(&mut self, x: f32, freq: f32, mix: f32) -> f32 {
        if mix <= 0.0 {
            return x;
        }
        self.phase += 2.0 * PI * freq / self.sample_rate;
        if self.phase > 2.0 * PI {
            self.phase -= 2.0 * PI;
        }
        x * (1.0 - mix + mix * self.phase.sin())
    }
}

// ---------------------------------------------------------------------------
// Перевантаження (drive): м'яке обрізання через tanh — хрипкий голос.
// ---------------------------------------------------------------------------
pub fn drive(x: f32, amount: f32) -> f32 {
    if amount <= 0.0 {
        return x;
    }
    // Нормуємо так, щоб звичайна гучність мови (~0.3) лишалась тією ж,
    // а змінювався лише «характер» звуку.
    const SPEECH: f32 = 0.3;
    let g = 1.0 + amount * 25.0;
    (g * x).tanh() * SPEECH / (g * SPEECH).tanh()
}

// ---------------------------------------------------------------------------
// Біткрашер: менше біт і менша частота дискретизації → звук старих ігор.
// ---------------------------------------------------------------------------
#[derive(Default)]
pub struct BitCrusher {
    counter: u32,
    held: f32,
}

impl BitCrusher {
    pub fn process(&mut self, x: f32, bits: u32, rate_div: u32) -> f32 {
        let mut y = x;
        if rate_div > 1 {
            if self.counter == 0 {
                self.held = x;
            }
            self.counter = (self.counter + 1) % rate_div;
            y = self.held;
        }
        if bits < 16 {
            let levels = (1u32 << (bits.max(1) - 1)) as f32;
            y = (y * levels).round() / levels;
        }
        y
    }
}

// ---------------------------------------------------------------------------
// Луна.
// ---------------------------------------------------------------------------
pub struct Echo {
    buf: Vec<f32>,
    pos: usize,
    sample_rate: f32,
}

impl Echo {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            buf: vec![0.0; sample_rate as usize + 1],
            pos: 0,
            sample_rate,
        }
    }

    pub fn process(&mut self, x: f32, delay_ms: f32, feedback: f32, mix: f32) -> f32 {
        if mix <= 0.0 {
            // Буфер усе одно пишемо, щоб луна не «вистрибувала» зі старим звуком.
            self.buf[self.pos] = 0.0;
            self.pos = (self.pos + 1) % self.buf.len();
            return x;
        }
        let len = self.buf.len();
        let delay = ((delay_ms / 1000.0 * self.sample_rate) as usize).clamp(1, len - 1);
        let read = (self.pos + len - delay) % len;
        let delayed = self.buf[read];
        self.buf[self.pos] = x + delayed * feedback.clamp(0.0, 0.95);
        self.pos = (self.pos + 1) % len;
        x + delayed * mix
    }
}

// ---------------------------------------------------------------------------
// Реверберація: спрощений Freeverb (4 гребінчасті + 2 all-pass фільтри).
// ---------------------------------------------------------------------------
struct Comb {
    buf: Vec<f32>,
    pos: usize,
    store: f32,
}

struct AllPass {
    buf: Vec<f32>,
    pos: usize,
}

pub struct Reverb {
    combs: Vec<Comb>,
    allpasses: Vec<AllPass>,
}

impl Reverb {
    pub fn new(sample_rate: f32) -> Self {
        let scale = sample_rate / 44_100.0;
        let len = |l: usize| ((l as f32 * scale) as usize).max(1);
        Self {
            combs: [1116, 1188, 1277, 1356, 1422, 1491]
                .iter()
                .map(|&l| Comb {
                    buf: vec![0.0; len(l)],
                    pos: 0,
                    store: 0.0,
                })
                .collect(),
            allpasses: [556, 441, 341]
                .iter()
                .map(|&l| AllPass {
                    buf: vec![0.0; len(l)],
                    pos: 0,
                })
                .collect(),
        }
    }

    pub fn process(&mut self, x: f32, size: f32, mix: f32) -> f32 {
        if mix <= 0.0 {
            return x;
        }
        let feedback = 0.7 + size.clamp(0.0, 1.0) * 0.28;
        let damp = 0.25;
        let input = x * 0.02;
        let mut wet = 0.0;
        for c in &mut self.combs {
            let out = c.buf[c.pos];
            c.store = out * (1.0 - damp) + c.store * damp;
            c.buf[c.pos] = input + c.store * feedback;
            c.pos = (c.pos + 1) % c.buf.len();
            wet += out;
        }
        for a in &mut self.allpasses {
            let buffered = a.buf[a.pos];
            let out = buffered - wet;
            a.buf[a.pos] = wet + buffered * 0.5;
            a.pos = (a.pos + 1) % a.buf.len();
            wet = out;
        }
        x * (1.0 - mix * 0.5) + wet * mix * 3.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn biquad_lowpass_attenuates_highs() {
        let sr = 48_000.0;
        let mut lp = Biquad::default();
        lp.set_lowpass(sr, 1000.0);
        let mut peak: f32 = 0.0;
        for i in 0..4800 {
            let x = (2.0 * PI * 8000.0 * i as f32 / sr).sin();
            let y = lp.process(x);
            if i > 1000 {
                peak = peak.max(y.abs());
            }
        }
        assert!(peak < 0.05, "peak {peak}");
    }

    #[test]
    fn bitcrusher_off_is_transparent() {
        let mut c = BitCrusher::default();
        assert_eq!(c.process(0.123, 16, 1), 0.123);
    }

    #[test]
    fn reverb_is_stable() {
        let mut r = Reverb::new(48_000.0);
        let mut max: f32 = 0.0;
        for i in 0..96_000 {
            let x = if i == 0 { 1.0 } else { 0.0 };
            max = max.max(r.process(x, 1.0, 1.0).abs());
        }
        assert!(max.is_finite() && max < 2.0, "max {max}");
    }

    #[test]
    fn gate_closes_on_silence() {
        let mut g = NoiseGate::new(48_000.0);
        let mut y = 0.0;
        for _ in 0..48_000 {
            y = g.process(0.001, -40.0);
        }
        assert!(y.abs() < 1e-4);
    }
}
