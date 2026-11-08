//! Low-latency 2x oversampling: a polyphase IIR half-band filter (two parallel chains of
//! first-order allpass sections, after Laurent de Soras' HIIR). Coefficients come from the
//! elliptic half-band design. The phase is not linear, but the delay is only a few samples.

use crate::util::flush;
use std::f64::consts::PI;

fn acc_num(q: f64, order: usize, c: usize) -> f64 {
    let (mut acc, mut sign, mut i) = (0.0, 1.0, 0usize);
    loop {
        let term = q.powi((i * (i + 1)) as i32) * (((i * 2 + 1) * c) as f64 * PI / order as f64).sin() * sign;
        acc += term;
        sign = -sign;
        i += 1;
        if term.abs() <= 1e-100 || i > 64 {
            return acc;
        }
    }
}

fn acc_den(q: f64, order: usize, c: usize) -> f64 {
    let (mut acc, mut sign, mut i) = (0.0, -1.0, 1usize);
    loop {
        let term = q.powi((i * i) as i32) * ((i * 2 * c) as f64 * PI / order as f64).cos() * sign;
        acc += term;
        sign = -sign;
        i += 1;
        if term.abs() <= 1e-100 || i > 64 {
            return acc;
        }
    }
}

/// Allpass coefficients (ascending) of an `n`-coefficient half-band filter for a transition
/// bandwidth `transition` (fraction of the oversampled rate, 0..0.5).
pub fn halfband_coefs(n: usize, transition: f64) -> Vec<f64> {
    let t = transition.clamp(1e-4, 0.499);
    let k = (((1.0 - t * 2.0) * PI / 4.0).tan()).powi(2);
    let kksqrt = (1.0 - k * k).powf(0.25);
    let e = 0.5 * (1.0 - kksqrt) / (1.0 + kksqrt);
    let e4 = e.powi(4);
    let q = e * (1.0 + e4 * (2.0 + e4 * (15.0 + 150.0 * e4)));
    let order = n * 2 + 1;
    let q4 = q.powf(0.25);
    let mut out: Vec<f64> = (1..=n)
        .map(|c| {
            let ww = acc_num(q, order, c) * q4 / (acc_den(q, order, c) + 0.5);
            let wsq = ww * ww;
            let x = ((1.0 - wsq * k) * (1.0 - wsq / k)).max(0.0).sqrt() / (1.0 + wsq);
            (1.0 - x) / (1.0 + x)
        })
        .collect();
    out.sort_by(f64::total_cmp);
    out
}

/// Group delay at DC of the up + down chain, in base-rate samples.
pub fn chain_latency(coefs: &[f64]) -> f64 {
    coefs.iter().map(|a| (1.0 - a) / (1.0 + a)).sum::<f64>() + 0.5
}

/// Settings Grit uses: coefficient count and transition (fraction of the oversampled rate).
pub const GRIT_N: usize = 7;
pub const GRIT_TRANSITION: f64 = 1.0 / 24.0;

/// One chain of first-order allpass sections `y = a (x - y1) + x1`.
#[derive(Clone)]
struct Chain {
    a: Vec<f32>,
    x1: Vec<f32>,
    y1: Vec<f32>,
}

impl Chain {
    fn new(a: Vec<f32>) -> Self {
        let n = a.len();
        Self { a, x1: vec![0.0; n], y1: vec![0.0; n] }
    }

    fn reset(&mut self) {
        self.x1.iter_mut().for_each(|v| *v = 0.0);
        self.y1.iter_mut().for_each(|v| *v = 0.0);
    }

    #[inline]
    fn process(&mut self, mut x: f32) -> f32 {
        for ((a, x1), y1) in self.a.iter().zip(self.x1.iter_mut()).zip(self.y1.iter_mut()) {
            let y = flush(a * (x - *y1) + *x1);
            *x1 = x;
            *y1 = y;
            x = y;
        }
        x
    }
}

fn split(coefs: &[f64]) -> (Chain, Chain) {
    let even = coefs.iter().step_by(2).map(|&c| c as f32).collect();
    let odd = coefs.iter().skip(1).step_by(2).map(|&c| c as f32).collect();
    (Chain::new(even), Chain::new(odd))
}

/// One input sample to two output samples (zero-stuffed half-band, unity gain).
#[derive(Clone)]
pub struct Upsampler2x {
    a: Chain,
    b: Chain,
}

impl Upsampler2x {
    pub fn new(coefs: &[f64]) -> Self {
        let (a, b) = split(coefs);
        Self { a, b }
    }

    pub fn reset(&mut self) {
        self.a.reset();
        self.b.reset();
    }

    /// Returns the two oversampled samples, in time order.
    #[inline]
    pub fn process(&mut self, x: f32) -> (f32, f32) {
        (self.a.process(x), self.b.process(x))
    }
}

/// Two input samples (time order) to one output sample.
#[derive(Clone)]
pub struct Downsampler2x {
    a: Chain,
    b: Chain,
    prev: f32,
}

impl Downsampler2x {
    pub fn new(coefs: &[f64]) -> Self {
        let (a, b) = split(coefs);
        Self { a, b, prev: 0.0 }
    }

    pub fn reset(&mut self) {
        self.a.reset();
        self.b.reset();
        self.prev = 0.0;
    }

    #[inline]
    pub fn process(&mut self, first: f32, second: f32) -> f32 {
        let y = 0.5 * (self.a.process(first) + self.prev);
        self.prev = self.b.process(second);
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    /// Amplitude and phase (radians) of `f` in `y`, skipping the first `skip` samples.
    fn tone(y: &[f32], f: f64, rate: f64, skip: usize) -> (f64, f64) {
        let (mut re, mut im) = (0.0, 0.0);
        for (i, v) in y.iter().enumerate().skip(skip) {
            let w = std::f64::consts::TAU * f * i as f64 / rate;
            re += f64::from(*v) * w.cos();
            im -= f64::from(*v) * w.sin();
        }
        let n = (y.len() - skip) as f64;
        ((re * re + im * im).sqrt() * 2.0 / n, im.atan2(re))
    }

    fn chain(coefs: &[f64], x: &[f32]) -> Vec<f32> {
        let (mut u, mut d) = (Upsampler2x::new(coefs), Downsampler2x::new(coefs));
        x.iter()
            .map(|&v| {
                let (a, b) = u.process(v);
                d.process(a, b)
            })
            .collect()
    }

    fn sine(f: f64, n: usize) -> Vec<f32> {
        (0..n).map(|i| (std::f64::consts::TAU * f * i as f64 / SR).sin() as f32).collect()
    }

    /// Chain gain in dB for a tone at `f` (48 kHz base).
    fn gain_db(coefs: &[f64], f: f64) -> f64 {
        let y = chain(coefs, &sine(f, 48_000));
        20.0 * tone(&y, f, SR, 4000).0.log10()
    }

    /// Level (dB) of the mirror image at 48k - f in the 2x output for a base-rate tone at `f`.
    fn up_image_db(coefs: &[f64], f: f64) -> f64 {
        let x = sine(f, 48_000);
        let mut u = Upsampler2x::new(coefs);
        let mut hi = Vec::with_capacity(x.len() * 2);
        for &v in &x {
            let (a, b) = u.process(v);
            hi.push(a);
            hi.push(b);
        }
        20.0 * tone(&hi, 48_000.0 - f, 2.0 * SR, 8000).0.max(1e-12).log10()
    }

    fn pick() -> Vec<f64> {
        halfband_coefs(GRIT_N, GRIT_TRANSITION)
    }

    #[test]
    #[ignore = "design scan, prints a table"]
    fn explore() {
        for n in 6..=12 {
            for t in [0.035, 0.0417, 0.05, 0.0625] {
                let c = halfband_coefs(n, t);
                let worst = [1000.0, 10_000.0, 20_000.0, 22_000.0].iter().map(|&f| gain_db(&c, f)).fold(0.0f64, |a, b| if b.abs() > a.abs() { b } else { a });
                let img = [15_000.0, 20_000.0, 22_000.0].iter().map(|&f| up_image_db(&c, f)).fold(-200.0, f64::max);
                println!("n {n} t {t}: worst pass {worst:.3} dB, img(26k+) {img:.1} dB, lat {:.2}", chain_latency(&c));
            }
        }
    }

    #[test]
    fn passband_is_flat_to_20k() {
        let c = pick();
        for f in [1000.0, 10_000.0, 20_000.0] {
            let g = gain_db(&c, f);
            println!("pass {f}: {g:.4} dB");
            assert!(g.abs() <= 0.1, "{f}: {g}");
        }
    }

    #[test]
    fn images_are_rejected() {
        let c = pick();
        // Input at f makes an image at 48k - f. The base-rate stopband starts near 26k (image of
        // 22k); everything above is 75 dB down (7 coefficients, transition 1/24).
        for f in [15_000.0, 20_000.0, 22_000.0] {
            let r = up_image_db(&c, f);
            println!("image of {f}: {r:.1} dB");
            assert!(r < -75.0, "{f}: {r}");
        }
        // Down: a tone at the 2x rate above 26k, fed as a stream, must not alias.
        let mut d = Downsampler2x::new(&c);
        for f in [26_000.0, 30_000.0, 40_000.0, 47_000.0] {
            d.reset();
            let hi: Vec<f32> = (0..96_000).map(|i| (std::f64::consts::TAU * f * i as f64 / (2.0 * SR)).sin() as f32).collect();
            let lo: Vec<f32> = hi.chunks(2).map(|p| d.process(p[0], p[1])).collect();
            let e = (lo[4000..].iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / (lo.len() - 4000) as f64).sqrt();
            let db = 20.0 * e.max(1e-12).log10();
            println!("down {f}: rms {db:.1} dB");
            assert!(db < -70.0, "{f}: {db}");
        }
    }

    #[test]
    fn group_delay_matches_the_formula() {
        let c = pick();
        let x = sine(100.0, 48_000);
        let y = chain(&c, &x);
        let (_, p0) = tone(&y, 100.0, SR, 4000);
        let (_, p1) = tone(&x, 100.0, SR, 4000);
        let d = (p1 - p0).rem_euclid(std::f64::consts::TAU) / (std::f64::consts::TAU * 100.0 / SR);
        println!("measured delay {d:.3}, formula {:.3}", chain_latency(&c));
        assert!((d - chain_latency(&c)).abs() < 0.1, "{d}");
    }
}
