//! Grit: a multi-style drive unit. Signal order: Low Cut (12 dB/oct high-pass, optional Thump
//! resonant bump at the cutoff) -> Tone (gentle tilt) -> Drive (+ Punish, +20 dB) -> one of five
//! waveshaping styles -> High Cut (6 or 30 dB/oct low-pass) -> Output -> Mix with the dry input.
//! With Auto on, the level after the shaper is matched at a reference level, so the output gain
//! moves opposite to Drive; with it off, more Drive is louder.
//!
//! Every shaper has unit slope at the origin and the pre-gain bottoms out at 0.01, so at Drive 0
//! the wet path equals the input (within rounding) in every style. Asymmetric styles pass through
//! a DC blocker. The curves are textbook shapes (tanh, biased tanh, algebraic sigmoids), not
//! models of any particular device. Mix 0 returns the input (delayed by the latency; the output
//! gain only applies to the wet path).
//!
//! The shaper runs at 2x the sample rate (a polyphase IIR half-band, a few samples of latency) so
//! its harmonics do not fold back as aliasing. The dry path is delayed by the same latency, so
//! the Mix blend stays in phase.

use crate::halfband::{Downsampler2x, GRIT_N, GRIT_TRANSITION, Upsampler2x, chain_latency, halfband_coefs};
use crate::params::{Params, choice, lin, log, param_plumbing, toggle};
use crate::util::{DcBlocker, Smoother, flush, frames_in, sane_channels, sane_sr};
use crate::{Category, ParamInfo, Plugin, PluginInfo, Unit, db_to_gain};

/// Tone tilt pivot and range (+-6 dB at the extremes, one-pole slopes).
const TILT_HZ: f32 = 800.0;
const TILT_DB: f32 = 6.0;
/// Level at which Auto matches the driven signal to the undriven one.
const REF_LEVEL: f32 = 0.25;
/// Punish adds this much gain before the shaper.
pub const PUNISH_DB: f32 = 20.0;
/// High Cut at or above this is off.
pub const HIGH_CUT_OFF_HZ: f32 = 19_999.0;
/// Low Cut range.
pub const LOW_CUT_MAX_HZ: f32 = 400.0;

static GRIT_PARAMS: [ParamInfo; 11] = [
    choice("style", "Style", &["Tape", "Germanium", "Console", "Triode", "Pentode"], 0),
    lin("low_cut", "Low Cut", 0.0, LOW_CUT_MAX_HZ, 0.0, Unit::Hz),
    toggle("thump", "Thump", false),
    lin("tone", "Tone", -100.0, 100.0, 0.0, Unit::Percent),
    lin("drive", "Drive", 0.0, 100.0, 30.0, Unit::Percent),
    toggle("punish", "Punish", false),
    log("high_cut", "High Cut", 1000.0, 20000.0, 20000.0, Unit::Hz),
    toggle("steep", "Steep", false),
    lin("output", "Output", -24.0, 12.0, 0.0, Unit::Db),
    toggle("auto_gain", "Auto", false),
    lin("mix", "Mix", 0.0, 100.0, 100.0, Unit::Percent),
];

pub static GRIT_INFO: PluginInfo = PluginInfo {
    id: "grit",
    name: "Grit",
    short_name: "Grit",
    category: Category::Harmonic,
    params: &GRIT_PARAMS,
    is_instrument: false,
    audiosuite: true,
};

/// The five shaping curves. All pass through the origin with slope 1.
fn shape(style: usize, u: f32) -> f32 {
    match style {
        // Tape: smooth symmetric tanh.
        0 => u.tanh(),
        // Germanium: asymmetric, the negative half clips earlier and lower (even + odd).
        1 => {
            if u >= 0.0 {
                u.tanh()
            } else {
                0.6 * (u / 0.6).tanh()
            }
        }
        // Console: gentle algebraic sigmoid, wide knee, odd harmonics.
        2 => u / (1.0 + u * u).sqrt(),
        // Triode: tanh around an offset working point, mostly even harmonics.
        3 => {
            const B: f32 = 0.5;
            let t = B.tanh();
            ((u + B).tanh() - t) / (1.0 - t * t)
        }
        // Pentode: symmetric with a sharper knee, odd harmonics.
        _ => u / (1.0 + u.abs().powi(3)).cbrt(),
    }
}

fn is_asymmetric(style: usize) -> bool {
    style == 1 || style == 3
}

/// Pre-shaper gain for a Drive amount (0..1) and Punish: 0.01 at zero, about +26 dB at full,
/// +20 dB more with Punish.
pub fn drive_gain(drive: f32, punish: bool) -> f32 {
    let g = 0.01 + 20.0 * drive.clamp(0.0, 1.0).powf(1.6);
    if punish { g * db_to_gain(PUNISH_DB) } else { g }
}

/// The shaped sample for pre-gain `g`. Without `auto` the level rises with drive; with it, the
/// result is matched at `REF_LEVEL`.
fn drive_stage(style: usize, g: f32, x: f32, auto: bool) -> f32 {
    let y = shape(style, g * x);
    if auto {
        let r = 0.5 * (shape(style, g * REF_LEVEL).abs() + shape(style, -g * REF_LEVEL).abs());
        if r.abs() > 1e-6 { y * REF_LEVEL / r } else { x }
    } else {
        y / g.min(1.0)
    }
}

/// Chamberlin state-variable filter (the high-pass output), stable for cutoffs far below fs/6.
#[derive(Clone, Copy, Default)]
struct Svf {
    lp: f32,
    bp: f32,
}

impl Svf {
    fn high_pass(&mut self, x: f32, f: f32, damp: f32) -> f32 {
        self.lp = flush(self.lp + f * self.bp);
        let hp = x - self.lp - damp * self.bp;
        self.bp = flush(self.bp + f * hp);
        hp
    }
}

/// Dry delay ring size (a power of two above the latency).
const DRY_RING: usize = 8;

/// Latency of the oversampling chain in samples (its low-frequency group delay, rounded). The
/// same at every sample rate.
pub fn latency_samples() -> usize {
    (chain_latency(&halfband_coefs(GRIT_N, GRIT_TRANSITION)).round().max(0.0) as usize).min(DRY_RING - 1)
}

pub struct Grit {
    p: Params,
    sr: f32,
    up: Vec<Upsampler2x>,
    down: Vec<Downsampler2x>,
    /// Dry path delay per channel, written at `dry_pos`.
    dry_ring: Vec<[f32; DRY_RING]>,
    dry_pos: usize,
    /// Latency of the oversampling chain in samples.
    latency: usize,
    /// Oversampling on (tests turn it off to measure the aliasing it removes).
    oversample: bool,
    dc: Vec<DcBlocker>,
    svf: Vec<Svf>,
    tilt: Vec<f32>,
    cut: Vec<[f32; 5]>,
    drive: Smoother,
    tone: Smoother,
    mix: Smoother,
    out: Smoother,
    /// Low Cut: SVF coefficient (0 = off) and damping (lower with Thump).
    svf_f: f32,
    svf_damp: f32,
    tilt_k: f32,
    cut_k: f32,
}

impl Default for Grit {
    fn default() -> Self {
        Self::new()
    }
}

impl Grit {
    pub fn new() -> Self {
        let mut s = Self {
            p: Params::new(&GRIT_PARAMS),
            sr: 48_000.0,
            up: Vec::new(),
            down: Vec::new(),
            dry_ring: Vec::new(),
            dry_pos: 0,
            latency: latency_samples(),
            oversample: true,
            dc: Vec::new(),
            svf: Vec::new(),
            tilt: Vec::new(),
            cut: Vec::new(),
            drive: Smoother::new(1.0),
            tone: Smoother::new(0.0),
            mix: Smoother::new(1.0),
            out: Smoother::new(1.0),
            svf_f: 0.0,
            svf_damp: 1.414,
            tilt_k: 0.0,
            cut_k: 0.0,
        };
        s.update();
        s.snap();
        s
    }

    fn update(&mut self) {
        self.drive.set(drive_gain(self.p.v(4) / 100.0, self.p.on(5)));
        self.tone.set(self.p.v(3) / 100.0);
        self.mix.set(self.p.v(10) / 100.0);
        self.out.set(db_to_gain(self.p.v(8)));
        let lc = self.p.v(1);
        self.svf_f = if lc >= 1.0 { 2.0 * (std::f32::consts::PI * lc.min(LOW_CUT_MAX_HZ) / self.sr).sin() } else { 0.0 };
        self.svf_damp = if self.p.on(2) { 0.6 } else { std::f32::consts::SQRT_2 };
        self.tilt_k = 1.0 - (-std::f32::consts::TAU * TILT_HZ / self.sr).exp();
        let hc = self.p.v(6);
        self.cut_k = if hc >= HIGH_CUT_OFF_HZ {
            0.0
        } else {
            // Five cascaded poles sit about 0.51x lower at -3 dB; compensate when steep.
            let fc = if self.p.on(7) { hc * 1.96 } else { hc }.min(0.45 * self.sr);
            1.0 - (-std::f32::consts::TAU * fc / self.sr).exp()
        };
    }

    fn snap(&mut self) {
        for s in [&mut self.drive, &mut self.tone, &mut self.mix, &mut self.out] {
            s.snap();
        }
    }
}

impl Plugin for Grit {
    param_plumbing!(GRIT_INFO);

    fn prepare(&mut self, sample_rate: f32, _max_block: usize, channels: usize) {
        self.sr = sane_sr(sample_rate);
        let ch = sane_channels(channels);
        let coefs = halfband_coefs(GRIT_N, GRIT_TRANSITION);
        self.latency = latency_samples();
        self.up = vec![Upsampler2x::new(&coefs); ch];
        self.down = vec![Downsampler2x::new(&coefs); ch];
        self.dry_ring = vec![[0.0; DRY_RING]; ch];
        self.dc = vec![DcBlocker::new(3.0, self.sr); ch];
        self.svf = vec![Svf::default(); ch];
        self.tilt = vec![0.0; ch];
        self.cut = vec![[0.0; 5]; ch];
        for s in [&mut self.drive, &mut self.tone, &mut self.mix, &mut self.out] {
            s.set_time(20.0, self.sr);
        }
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.up.iter_mut().for_each(Upsampler2x::reset);
        self.down.iter_mut().for_each(Downsampler2x::reset);
        self.dry_ring.iter_mut().for_each(|r| *r = [0.0; DRY_RING]);
        self.dry_pos = 0;
        self.dc.iter_mut().for_each(DcBlocker::reset);
        self.svf.iter_mut().for_each(|s| *s = Svf::default());
        self.tilt.iter_mut().for_each(|v| *v = 0.0);
        self.cut.iter_mut().for_each(|v| *v = [0.0; 5]);
        self.snap();
    }

    fn latency(&self) -> usize {
        self.latency
    }

    fn process(&mut self, io: &mut [Vec<f32>], frames: usize) {
        let ch = self.dc.len().min(io.len());
        let frames = frames_in(io, frames, ch);
        let style = self.p.choice(0).min(4);
        let auto = self.p.on(9);
        let poles = if self.p.on(7) { 5 } else { 1 };
        let (sf, sd, tk, ck) = (self.svf_f, self.svf_damp, self.tilt_k, self.cut_k);
        let (lat, os) = (self.latency, self.oversample);
        for n in 0..frames {
            let g = self.drive.next();
            let t = self.tone.next();
            let m = self.mix.next();
            let o = self.out.next();
            let (g_lo, g_hi) = (db_to_gain(-t * TILT_DB), db_to_gain(t * TILT_DB));
            for c in 0..ch {
                let (Some(buf), Some(dc), Some(svf), Some(tl), Some(cut)) =
                    (io.get_mut(c), self.dc.get_mut(c), self.svf.get_mut(c), self.tilt.get_mut(c), self.cut.get_mut(c))
                else {
                    continue;
                };
                let (Some(up), Some(down), Some(ring)) = (self.up.get_mut(c), self.down.get_mut(c), self.dry_ring.get_mut(c)) else { continue };
                let Some(x) = buf.get_mut(n) else { continue };
                let input = if x.is_finite() { *x } else { 0.0 };
                if let Some(slot) = ring.get_mut(self.dry_pos & (DRY_RING - 1)) {
                    *slot = input;
                }
                let dry = ring.get(self.dry_pos.wrapping_sub(lat) & (DRY_RING - 1)).copied().unwrap_or(0.0);
                if m <= 0.0 {
                    *x = dry;
                    continue;
                }
                let mut y = input;
                if sf > 0.0 {
                    y = svf.high_pass(y, sf, sd);
                }
                if t != 0.0 {
                    *tl = flush(*tl + (y - *tl) * tk);
                    y = *tl * g_lo + (y - *tl) * g_hi;
                }
                y = if os {
                    let (a, b) = up.process(y);
                    down.process(drive_stage(style, g, a, auto), drive_stage(style, g, b, auto))
                } else {
                    drive_stage(style, g, y, auto)
                };
                if is_asymmetric(style) {
                    y = dc.process(y);
                }
                if ck > 0.0 {
                    for s in cut.iter_mut().take(poles) {
                        *s = flush(*s + (y - *s) * ck);
                        y = *s;
                    }
                }
                let wet = if y.is_finite() { y * o } else { 0.0 };
                *x = dry * (1.0 - m) + wet * m;
            }
            self.dry_pos = self.dry_pos.wrapping_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const N: usize = 9600;
    const STYLES: [&str; 5] = ["Tape", "Germanium", "Console", "Triode", "Pentode"];

    fn name(style: usize) -> &'static str {
        STYLES.get(style).copied().unwrap_or("?")
    }

    fn sine_at(f: f32, amp: f32) -> Vec<f32> {
        (0..N).map(|i| amp * (std::f32::consts::TAU * f * i as f32 / 48_000.0).sin()).collect()
    }

    fn sine(amp: f32) -> Vec<f32> {
        sine_at(220.0, amp)
    }

    fn make(style: usize, drive: f32, mix: f32, auto: bool) -> Grit {
        let mut g = Grit::new();
        g.prepare(48_000.0, 512, 2);
        g.set_param("style", style as f32);
        g.set_param("drive", drive);
        g.set_param("mix", mix);
        g.set_param("auto_gain", if auto { 1.0 } else { 0.0 });
        g.reset();
        g
    }

    fn run(g: &mut Grit, x: &[f32]) -> Vec<f32> {
        let mut io = vec![x.to_vec(), x.to_vec()];
        g.process(&mut io, x.len());
        io.swap_remove(0)
    }

    fn rms(x: &[f32]) -> f32 {
        (x[N / 2..].iter().map(|v| v * v).sum::<f32>() / (N / 2) as f32).sqrt()
    }

    #[test]
    fn finite_for_every_style_and_extreme() {
        for style in 0..5 {
            for (drive, auto, punish, steep) in [(0.0, false, 0.0, 0.0), (50.0, true, 0.0, 1.0), (100.0, false, 1.0, 0.0), (100.0, true, 1.0, 1.0)] {
                for amp in [0.0, 0.1, 1.0, 8.0] {
                    let mut g = make(style, drive, 100.0, auto);
                    g.set_param("punish", punish);
                    g.set_param("steep", steep);
                    g.set_param("high_cut", 3000.0);
                    g.set_param("low_cut", 120.0);
                    g.set_param("thump", 1.0);
                    g.set_param("tone", 80.0);
                    let mut x = sine(amp);
                    x[10] = f32::NAN;
                    x[11] = f32::INFINITY;
                    let y = run(&mut g, &x);
                    assert!(y.iter().all(|v| v.is_finite() && v.abs() < 40.0), "{} drive {drive} amp {amp}", name(style));
                }
            }
        }
    }

    #[test]
    fn unity_at_zero_drive_for_every_style() {
        for style in 0..5 {
            for auto in [false, true] {
                let x = sine(0.5);
                let y = run(&mut make(style, 0.0, 100.0, auto), &x);
                // The output is the input delayed by the oversampling latency.
                let lat = latency_samples();
                let err = (N / 2..N).map(|i| (x[i - lat] - y[i]).abs()).fold(0.0, f32::max);
                assert!(err < 1e-2, "{} auto {auto}: {err}", name(style));
            }
        }
    }

    #[test]
    fn mix_zero_is_the_dry_signal_bit_exact() {
        for style in 0..5 {
            let x = sine(0.7);
            let mut g = make(style, 80.0, 0.0, true);
            g.set_param("output", 9.0);
            g.set_param("low_cut", 300.0);
            g.set_param("tone", 100.0);
            let y = run(&mut g, &x);
            let lat = g.latency();
            assert!(lat > 0);
            let delayed = |i: usize| if i >= lat { x[i - lat] } else { 0.0 };
            assert!((0..x.len()).all(|i| y[i].to_bits() == delayed(i).to_bits()), "{}", name(style));
        }
    }

    #[test]
    fn drive_adds_level_and_distortion_when_auto_is_off() {
        for style in 0..5 {
            let x = sine(0.1);
            let soft = rms(&run(&mut make(style, 5.0, 100.0, false), &x));
            let hot = rms(&run(&mut make(style, 90.0, 100.0, false), &x));
            assert!(hot > soft * 1.5, "{}: {soft} -> {hot}", name(style));
        }
    }

    #[test]
    fn auto_keeps_the_level_roughly_constant_across_drive() {
        for style in 0..5 {
            let x = sine(0.25);
            let base = rms(&run(&mut make(style, 0.0, 100.0, true), &x));
            for drive in [20.0, 50.0, 80.0, 100.0] {
                let r = rms(&run(&mut make(style, drive, 100.0, true), &x)) / base;
                assert!((0.7..1.6).contains(&r), "{} drive {drive}: x{r}", name(style));
            }
        }
    }

    #[test]
    fn punish_adds_drive() {
        for style in 0..5 {
            let x = sine(0.05);
            let a = rms(&run(&mut make(style, 20.0, 100.0, false), &x));
            let mut g = make(style, 20.0, 100.0, false);
            g.set_param("punish", 1.0);
            g.reset();
            assert!(rms(&run(&mut g, &x)) > a * 2.0, "{}", name(style));
        }
    }

    #[test]
    fn styles_sound_different() {
        let x = sine(0.4);
        let outs: Vec<Vec<f32>> = (0..5).map(|s| run(&mut make(s, 70.0, 100.0, false), &x)).collect();
        for a in 0..5 {
            for b in a + 1..5 {
                let d = outs[a].iter().zip(&outs[b]).map(|(p, q)| (p - q).abs()).fold(0.0, f32::max);
                assert!(d > 0.01, "{} vs {}", STYLES[a], STYLES[b]);
            }
        }
    }

    #[test]
    fn triode_adds_even_and_pentode_odd_harmonics() {
        // DC-free second harmonic share vs third, via correlation with the harmonics.
        let harm = |style: usize, k: f32| {
            let x = sine(0.3);
            let y = run(&mut make(style, 60.0, 100.0, false), &x);
            let (mut re, mut im) = (0.0f32, 0.0f32);
            for (i, v) in y.iter().enumerate().skip(N / 2) {
                let ph = std::f32::consts::TAU * 220.0 * k * i as f32 / 48_000.0;
                re += v * ph.cos();
                im += v * ph.sin();
            }
            (re * re + im * im).sqrt()
        };
        assert!(harm(3, 2.0) > 5.0 * harm(4, 2.0), "even: triode vs pentode");
        assert!(harm(4, 3.0) > harm(3, 3.0) * 0.5, "odd: pentode keeps the third");
    }

    #[test]
    fn low_cut_removes_lows_and_high_cut_removes_highs() {
        let low = sine_at(40.0, 0.3);
        let high = sine_at(10_000.0, 0.3);
        let mut a = make(0, 0.0, 100.0, false);
        let open_low = rms(&run(&mut a, &low));
        let mut b = make(0, 0.0, 100.0, false);
        b.set_param("low_cut", 300.0);
        assert!(rms(&run(&mut b, &low)) < open_low * 0.3);
        for (steep, max) in [(0.0, 0.5), (1.0, 0.05)] {
            let mut c = make(0, 0.0, 100.0, false);
            c.set_param("high_cut", 2000.0);
            c.set_param("steep", steep);
            let open = rms(&run(&mut make(0, 0.0, 100.0, false), &high));
            assert!(rms(&run(&mut c, &high)) < open * max, "steep {steep}");
        }
    }

    #[test]
    fn tone_tilts_the_spectrum() {
        let hi = sine_at(6000.0, 0.1);
        let mut dark = make(0, 0.0, 100.0, false);
        dark.set_param("tone", -100.0);
        let mut bright = make(0, 0.0, 100.0, false);
        bright.set_param("tone", 100.0);
        assert!(rms(&run(&mut bright, &hi)) > 2.0 * rms(&run(&mut dark, &hi)));
    }

    /// Everything except the `f` fundamental (fitted and subtracted), as an RMS.
    fn residual(y: &[f32], f: f32) -> f32 {
        let tail = &y[N / 2..];
        let (mut re, mut im) = (0.0f64, 0.0f64);
        let w = |i: usize| std::f64::consts::TAU * f64::from(f) * (i + N / 2) as f64 / 48_000.0;
        for (i, v) in tail.iter().enumerate() {
            re += f64::from(*v) * w(i).cos();
            im += f64::from(*v) * w(i).sin();
        }
        let (a, b) = (2.0 * re / tail.len() as f64, 2.0 * im / tail.len() as f64);
        let e: f64 = tail.iter().enumerate().map(|(i, v)| (f64::from(*v) - a * w(i).cos() - b * w(i).sin()).powi(2)).sum();
        (e / tail.len() as f64).sqrt() as f32
    }

    #[test]
    fn oversampling_cuts_aliasing() {
        // Pentode, high drive, a -20 dBFS 15 kHz tone. (Driven into full clipping at 0 dBFS, 2x only
        // buys about 3 dB: a square wave's harmonics hardly decay.)
        let x = sine_at(15_000.0, 0.1);
        let mut on = make(4, 80.0, 100.0, false);
        let mut off = make(4, 80.0, 100.0, false);
        off.oversample = false;
        let (a_on, a_off) = (residual(&run(&mut on, &x), 15_000.0), residual(&run(&mut off, &x), 15_000.0));
        let db = 20.0 * (a_off / a_on).log10();
        println!("alias residual: oversampled {:.1} dB, plain {:.1} dB, {db:.1} dB less", 20.0 * a_on.log10(), 20.0 * a_off.log10());
        assert!(db >= 15.0, "only {db} dB less aliasing");
    }

    #[test]
    fn registered() {
        assert!(crate::plugin_info("grit").is_some());
        assert_eq!(GRIT_INFO.params.len(), 11);
    }
}
