//! Black-box probe of a Relic plugin's oversampling: reported latency, impulse response shape
//! (pre-ring = linear phase, none = minimum phase / IIR), phase delay per frequency, and alias
//! residual of a hot 15 kHz sine. Usage: relic_os <vst3 path> <name substring>
use soundcraft_dsp::Plugin;
use std::path::PathBuf;

const SR: f32 = 48_000.0;
const OS: &str = "1769509718";
const LL: &str = "1579976837";

fn run(p: &mut dyn Plugin, x: &[f32]) -> Vec<f32> {
    let mut out = Vec::with_capacity(x.len());
    for c in x.chunks(512) {
        let mut io = vec![c.to_vec(), c.to_vec()];
        p.process(&mut io, c.len());
        out.extend_from_slice(&io[0]);
    }
    out
}

/// Amplitude and phase (radians) of frequency f in y[a..b].
fn tone(y: &[f32], f: f32) -> (f32, f32) {
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (i, v) in y.iter().enumerate() {
        let w = std::f64::consts::TAU * f64::from(f) * i as f64 / f64::from(SR);
        re += f64::from(*v) * w.cos();
        im -= f64::from(*v) * w.sin();
    }
    let n = y.len() as f64;
    (((re * re + im * im).sqrt() * 2.0 / n) as f32, im.atan2(re) as f32)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let dir = PathBuf::from(&a[1]);
    let found = soundcraft_vst3_host::scan_paths(std::slice::from_ref(&dir));
    soundcraft_vst3_host::add_search_dir(&dir);
    let d = found.iter().find(|d| d.name.to_lowercase().contains(&a[2].to_lowercase())).expect("plugin");
    let extra: Vec<(String, f32)> = a.iter().skip(3).filter_map(|s| s.split_once('=').map(|(k, v)| (k.to_string(), v.parse().unwrap()))).collect();
    for ll in [0.0f32, 1.0] {
        for os in [0.0f32, 1.0, 2.0] {
            let mk = || {
                let mut p = soundcraft_vst3_host::create(&d.id).expect("create");
                p.prepare(SR, 512, 2);
                p.set_param(OS, os);
                p.set_param(LL, ll);
                for (k, v) in &extra {
                    p.set_param(k, *v);
                }
                run(p.as_mut(), &vec![0.0; 4096]);
                p.prepare(SR, 512, 2);
                // Let parameter smoothing settle.
                run(p.as_mut(), &vec![0.0; 48_000]);
                p
            };
            let mut p = mk();
            let lat = p.latency();
            // Impulse at -40 dBFS.
            let mut x = vec![0.0f32; 8192];
            x[1000] = 0.01;
            let y = run(p.as_mut(), &x);
            let (pk, _) = y.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap();
            let e = |r: std::ops::Range<usize>| y[r].iter().map(|v| v * v).sum::<f32>();
            let pre = e(1000..pk) / e(1000..8192).max(1e-30);
            let taps: Vec<String> = (pk.saturating_sub(3)..pk + 6).map(|i| format!("{:+.3}", y[i] / y[pk])).collect();
            // Phase delay at a few frequencies, low level sines.
            let mut gd = Vec::new();
            for f in [1000.0f32, 5000.0, 10000.0, 15000.0, 19000.0] {
                let mut p = mk();
                let x: Vec<f32> = (0..24_000).map(|i| 0.01 * (std::f32::consts::TAU * f * i as f32 / SR).sin()).collect();
                let y = run(p.as_mut(), &x);
                let (ai, phi) = tone(&x[12_000..], f);
                let (ao, pho) = tone(&y[12_000..], f);
                let mut dp = phi - pho;
                // Unwrap with the reported latency as the first guess.
                let guess = std::f32::consts::TAU * f * lat as f32 / SR;
                while dp < guess - std::f32::consts::PI {
                    dp += std::f32::consts::TAU;
                }
                while dp > guess + std::f32::consts::PI {
                    dp -= std::f32::consts::TAU;
                }
                gd.push(format!("{}k:{:.2}smp/{:+.1}dB", f / 1000.0, dp / std::f32::consts::TAU * SR / f, 20.0 * (ao / ai).log10()));
            }
            // Alias residual: hot 15 kHz, everything in band except 15 kHz is alias (harmonics are > 24 kHz).
            let mut p = mk();
            let x: Vec<f32> = (0..48_000).map(|i| 0.9 * (std::f32::consts::TAU * 15_000.0 * i as f32 / SR).sin()).collect();
            let y = run(p.as_mut(), &x);
            let s = &y[24_000..];
            let (am, ph) = tone(s, 15_000.0);
            let res: f32 = s.iter().enumerate().map(|(i, v)| v - am * (std::f32::consts::TAU * 15_000.0 * i as f32 / SR + ph).cos()).map(|r| r * r).sum::<f32>() / s.len() as f32;
            let alias_db = 10.0 * (res / (am * am / 2.0)).log10();
            println!("LL={ll} OS={os} lat={lat} peak@{} pre-ring={:.1}% taps[{}] | {} | alias {alias_db:.1} dBc", pk as i64 - 1000, pre * 100.0, taps.join(" "), gd.join(" "));
        }
    }
}
