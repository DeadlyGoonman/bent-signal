//! Retune-speed test bench: synthetic voice (steps and vibrato) through Plumb, Xpitch and
//! Auto-Tune Pro (VST3 via soundcraft-vst3-host), pitch tracked with YIN.
//! Usage: rate step|vib|scan
use soundcraft_dsp::Plugin;
use soundcraft_dsp::pitch_detect::{YinScratch, yin_with};
use soundcraft_dsp::plugins::tune::Tune;
use std::path::PathBuf;

const SR: f32 = 48_000.0;
const TAU: f32 = std::f32::consts::TAU;

/// A harmonic voice following `midi(t)`.
fn voice(secs: f32, midi: impl Fn(f32) -> f32) -> Vec<f32> {
    let n = (secs * SR) as usize;
    let mut ph = 0.0f64;
    (0..n)
        .map(|i| {
            let t = i as f32 / SR;
            let f = 440.0 * 2f32.powf((midi(t) - 69.0) / 12.0);
            ph += f64::from(f / SR);
            let mut s = 0.0f32;
            for h in 1..=24 {
                let a = 1.0 / (h as f32).powf(1.1);
                s += a * (std::f64::consts::TAU * ph * f64::from(h)).sin() as f32;
            }
            let env = (t / 0.02).min(1.0) * ((secs - t) / 0.05).clamp(0.0, 1.0);
            0.25 * s * env
        })
        .collect()
}

fn run(p: &mut dyn Plugin, x: &[f32]) -> Vec<f32> {
    let lat = p.latency();
    let total = x.len() + lat + 4800;
    let mut out = Vec::with_capacity(total);
    let mut i = 0;
    while i < total {
        let n = 512.min(total - i);
        let blk: Vec<f32> = (i..i + n).map(|k| x.get(k).copied().unwrap_or(0.0)).collect();
        let mut io = vec![blk.clone(), blk];
        p.process(&mut io, n);
        out.extend_from_slice(&io[0]);
        i += n;
    }
    out.split_off(lat.min(out.len()))
}

/// Pitch track in MIDI notes: (time of window centre, midi) every 1.25 ms over 10 ms windows.
fn track(y: &[f32]) -> Vec<(f32, f32)> {
    let (w, hop) = (480usize, 60usize);
    let mut sc = YinScratch::default();
    sc.reserve(1248);
    let mut v = Vec::new();
    let mut i = 0;
    while i + w <= y.len() {
        let fr = &y[i..i + w];
        let rms = (fr.iter().map(|s| s * s).sum::<f32>() / w as f32).sqrt();
        if rms > 0.003 {
            if let Some(f) = yin_with(fr, SR, 150.0, 900.0, 0.15, &mut sc) {
                v.push(((i + w / 2) as f32 / SR, 69.0 + 12.0 * (f / 440.0).log2()));
            }
        }
        i += hop;
    }
    v
}

enum Kind {
    Plumb(f32),
    Xpitch(f32),
    Atp(f32),
}

struct Ctx {
    xp: soundcraft_vst3_host::Vst3Descriptor,
    atp: soundcraft_vst3_host::Vst3Descriptor,
    atp_scale: f32,
}

fn render(c: &Ctx, k: &Kind, x: &[f32]) -> Vec<f32> {
    match k {
        Kind::Plumb(speed) => {
            let mut t = Tune::new();
            t.prepare(SR, 512, 2);
            t.set_param("key", 0.0);
            t.set_param("scale", 1.0);
            t.set_param("speed", *speed);
            t.set_param("flex", 0.0);
            run(&mut t, x)
        }
        Kind::Xpitch(tr) => {
            let mut p = soundcraft_vst3_host::create(&c.xp.id).expect("xpitch");
            p.prepare(SR, 512, 2);
            p.set_param("11040722", 2.0);
            p.set_param("322477887", 0.0);
            p.set_param("322547095", 0.0);
            p.set_param("441993146", *tr);
            run(p.as_mut(), x)
        }
        Kind::Atp(n) => {
            let mut p = soundcraft_vst3_host::create(&c.atp.id).expect("atp");
            p.prepare(SR, 512, 2);
            p.set_param(&std::env::var("ATP_SCALE_ID").unwrap_or("1".into()), c.atp_scale);
            p.set_param("2", 0.0);
            p.set_param("4", *n);
            p.set_param("90", 0.0);
            run(p.as_mut(), x)
        }
    }
}

/// ATP normalized value for a retune time in ms, from the plugin's own text.
fn atp_norm(c: &Ctx, ms: f32) -> f32 {
    let mut p = soundcraft_vst3_host::create(&c.atp.id).expect("atp");
    let mut ed = p.editor().expect("editor");
    let mut txt = |v: f32| -> f32 { ed.value_text("4", v).and_then(|s| s.trim_end_matches(" ms").parse().ok()).unwrap_or(0.0) };
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..30 {
        let mid = 0.5 * (lo + hi);
        if txt(mid) > ms {
            lo = mid
        } else {
            hi = mid
        }
    }
    0.5 * (lo + hi)
}

fn settings(c: &Ctx, speeds: &[f32]) -> Vec<(String, Kind)> {
    let mut v = Vec::new();
    for &s in speeds {
        v.push((format!("plumb_{s}"), Kind::Plumb(s)));
    }
    for &s in speeds {
        v.push((format!("atp_{s}ms"), Kind::Atp(atp_norm(c, s))));
    }
    for t in [0.0f32, 0.1, 0.15, 0.2, 0.3, 0.5, 0.7, 1.0] {
        v.push((format!("xpitch_T{t}"), Kind::Xpitch(t)));
    }
    v
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let mode = a.get(1).map(String::as_str).unwrap_or("step");
    let dirs = [r"C:\Program Files\Common Files\VST3\Nuro Audio", r"C:\Program Files\Common Files\VST3\Antares"];
    let found = soundcraft_vst3_host::scan_paths(&dirs.iter().map(PathBuf::from).collect::<Vec<_>>());
    for d in dirs {
        soundcraft_vst3_host::add_search_dir(std::path::Path::new(d));
    }
    let pick = |n: &str| found.iter().find(|d| d.name.to_lowercase().contains(n)).cloned().expect(n);
    let atp_scale: f32 = std::env::var("ATP_SCALE").ok().and_then(|s| s.parse().ok()).unwrap_or(2.0);
    let c = Ctx { xp: pick("xpitch"), atp: pick("auto-tune pro"), atp_scale };

    if mode == "dbg" {
        dbg(&c);
        return;
    }
    if mode == "trace" {
        let l = vec![("plumb_20".to_string(), Kind::Plumb(20.0)), ("plumb_100".to_string(), Kind::Plumb(100.0)), ("atp_20".to_string(), Kind::Atp(atp_norm(&c, 20.0))), ("atp_100".to_string(), Kind::Atp(atp_norm(&c, 100.0))), ("xp_0.2".to_string(), Kind::Xpitch(0.2))];
        trace(&c, &l);
        return;
    }
    if mode == "scan" {
        // D#4 exactly: Chromatic keeps it, Major in C moves it to D or E.
        let x = voice(0.8, |_| 63.4);
        for s in 0..=28 {
            let cc = Ctx { xp: c.xp.clone(), atp: c.atp.clone(), atp_scale: s as f32 };
            let y = render(&cc, &Kind::Atp(1.0), &x);
            let tail: Vec<f32> = track(&y).iter().filter(|(t, _)| *t > 0.5).map(|(_, m)| *m).collect();
            let m = tail.iter().sum::<f32>() / tail.len().max(1) as f32;
            println!("atp scale {s}: D#4 -> {m:.2}");
        }
        return;
    }

    let speeds = [0.0f32, 5.0, 10.0, 15.0, 20.0, 30.0, 50.0, 75.0, 100.0, 150.0, 200.0, 400.0];
    if mode == "step" {
        // E4 in tune, then +40 ct, alternating every 0.6 s.
        let step = 0.6f32;
        let dev = |t: f32| if ((t / step) as i32) % 2 == 1 { 0.40 } else { 0.0 };
        let x = voice(step * 8.0, |t| 64.0 + dev(t));
        println!("{:<14} {:>7} {:>7} {:>7} {:>8}", "setting", "t10ms", "t63ms", "t90ms", "overshoot");
        for (name, k) in settings(&c, &speeds) {
            let y = render(&c, &k, &x);
            let tr = track(&y);
            let (mut t63, mut t90, mut t10, mut over, mut n) = (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0);
            for ev in 2..8 {
                let t0 = ev as f32 * step;
                let seg: Vec<(f32, f32)> = tr.iter().filter(|(t, _)| *t >= t0 && *t < t0 + step + 0.3).map(|(t, m)| (*t, (m - 64.0) * 100.0)).collect();
                let sgn = if ev % 2 == 1 { 1.0 } else { -1.0 };
                // The first sample where the output carries the step (>= 28 of 40 ct away from 0 in the event's direction).
                let Some(p0) = seg.iter().position(|(_, e)| e * sgn >= 28.0) else { continue };
                let base = seg[p0].0;
                let peak = seg[p0].1 * sgn;
                let first = |th: f32| seg[p0..].iter().find(|(_, e)| e * sgn <= th).map(|(t, _)| (t - base) * 1000.0);
                if let (Some(a1), Some(a63), Some(a90)) = (first(peak * 0.9), first(peak * 0.37), first(peak * 0.1)) {
                    t10 += a1;
                    t63 += a63;
                    t90 += a90;
                    over += seg[p0..].iter().map(|(_, e)| (-e * sgn).max(0.0)).fold(0.0, f32::max);
                    n += 1;
                }
            }
            let n = n.max(1) as f32;
            println!("{name:<14} {:>7.1} {:>7.1} {:>7.1} {:>8.1}", t10 / n, t63 / n, t90 / n, over / n);
        }
    }
    if mode == "vib" {
        // E4 + 10 ct, 5.5 Hz vibrato of +-35 ct plus a 0.4 Hz drift of +-12 ct: how much vibrato survives, and how far from the note.
        let x = voice(4.0, |t| 64.0 + 0.10 + 0.35 * (TAU * 5.5 * t).sin() + 0.12 * (TAU * 0.4 * t).sin());
        println!("{:<14} {:>9} {:>9} {:>9}", "setting", "vib_kept%", "mean_ct", "abs_ct");
        for (name, k) in settings(&c, &speeds) {
            let y = render(&c, &k, &x);
            let tr: Vec<(f32, f32)> = track(&y).into_iter().filter(|(t, _)| *t > 1.0 && *t < 3.6).collect();
            let cents: Vec<f32> = tr.iter().map(|(_, m)| (m - 64.0) * 100.0).collect();
            let cnt = cents.len().max(1) as f32;
            let mean = cents.iter().sum::<f32>() / cnt;
            let (mut s, mut co) = (0.0f32, 0.0f32);
            for ((t, _), e) in tr.iter().zip(&cents) {
                s += e * (TAU * 5.5 * t).sin();
                co += e * (TAU * 5.5 * t).cos();
            }
            let amp = 2.0 * (s * s + co * co).sqrt() / cnt;
            let abs = cents.iter().map(|e| e.abs()).sum::<f32>() / cnt;
            println!("{name:<14} {:>9.0} {:>9.1} {:>9.1}", 100.0 * amp / 35.0, mean, abs);
        }
    }
}

/// Debug: ATP output vs input.
pub fn dbg(c: &Ctx) {
    let x = voice(1.5, |_| 63.4);
    let mut p = soundcraft_vst3_host::create(&c.atp.id).expect("atp");
    p.prepare(SR, 512, 2);
    println!("latency {}", p.latency());
    for (id, v) in [("1", 0.0f32), ("2", 0.0), ("4", 1.0)] {
        println!("set {id} {v} -> {}", p.set_param(id, v));
    }
    let y = run(p.as_mut(), &x);
    let e_in = x.iter().map(|s| s * s).sum::<f32>();
    let e_out = y.iter().take(x.len()).map(|s| s * s).sum::<f32>();
    println!("energy in {e_in} out {e_out}");
    let t = track(&y);
    for (tt, m) in t.iter().step_by(100) {
        println!("{tt:.3} {m:.2}");
    }
}

/// Error-vs-time after the step at 3 x 0.6 s, for a few settings, every 10 ms (offset so t=0 is the output's first >=28 ct sample).
pub fn trace(c: &Ctx, list: &[(String, Kind)]) {
    let step = 0.6f32;
    let dev = |t: f32| if ((t / step) as i32) % 2 == 1 { 0.40 } else { 0.0 };
    let x = voice(step * 6.0, |t| 64.0 + dev(t));
    let mut cols = Vec::new();
    for (name, k) in list {
        let tr = track(&render(c, k, &x));
        let ev: f32 = std::env::var("TRACE_EV").ok().and_then(|s| s.parse().ok()).unwrap_or(5.0);
        let sg = if ev as i32 % 2 == 1 { 1.0 } else { -1.0 };
        let t0 = ev * step;
        let seg: Vec<(f32, f32)> = tr.iter().filter(|(t, _)| *t >= t0 && *t < t0 + 0.55).map(|(t, m)| (*t, (m - 64.0) * 100.0)).collect();
        let base = seg.iter().find(|(_, e)| *e * sg >= 28.0).map(|(t, _)| *t).unwrap_or(t0);
        let col: Vec<f32> = (0..45).map(|i| { let tt = base + i as f32 * 0.01; seg.iter().min_by(|a, b| (a.0 - tt).abs().total_cmp(&(b.0 - tt).abs())).map(|s| s.1 * sg).unwrap_or(0.0) }).collect();
        cols.push((name.clone(), col));
    }
    print!("{:>6}", "ms");
    for (n, _) in &cols { print!(" {n:>12}"); }
    println!();
    for i in 0..45 {
        print!("{:>6}", i * 10);
        for (_, c) in &cols { print!(" {:>12.1}", c[i]); }
        println!();
    }
}
