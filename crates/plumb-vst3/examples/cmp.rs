//! Offline A/B: renders a WAV through Xpitch (VST3, in SoundCraft's host) and Plumb's DSP, with
//! latency compensated, to 32-bit float mono WAVs for waveform comparison.
//! Usage: cmp <in.wav> <out_dir> [start_s] [len_s]
use soundcraft_dsp::Plugin;
use soundcraft_dsp::plugins::tune::Tune;
use std::path::{Path, PathBuf};

fn read_wav(p: &Path) -> (u32, Vec<f32>) {
    let b = std::fs::read(p).expect("read");
    let (mut pos, mut sr) = (12usize, 48000u32);
    while pos + 8 <= b.len() {
        let id = &b[pos..pos + 4];
        let n = u32::from_le_bytes(b[pos + 4..pos + 8].try_into().unwrap()) as usize;
        if id == b"fmt " {
            sr = u32::from_le_bytes(b[pos + 12..pos + 16].try_into().unwrap());
        }
        if id == b"data" {
            let end = (pos + 8 + n).min(b.len());
            let v = b[pos + 8..end].chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
            return (sr, v);
        }
        pos += 8 + n + (n & 1);
    }
    panic!("no data");
}

fn write_wav(p: &Path, sr: u32, x: &[f32]) {
    let n = (x.len() * 4) as u32;
    let mut o = Vec::with_capacity(44 + x.len() * 4);
    o.extend_from_slice(b"RIFF");
    o.extend_from_slice(&(36 + n).to_le_bytes());
    o.extend_from_slice(b"WAVEfmt ");
    o.extend_from_slice(&16u32.to_le_bytes());
    o.extend_from_slice(&3u16.to_le_bytes());
    o.extend_from_slice(&1u16.to_le_bytes());
    o.extend_from_slice(&sr.to_le_bytes());
    o.extend_from_slice(&(sr * 4).to_le_bytes());
    o.extend_from_slice(&4u16.to_le_bytes());
    o.extend_from_slice(&32u16.to_le_bytes());
    o.extend_from_slice(b"data");
    o.extend_from_slice(&n.to_le_bytes());
    for v in x {
        o.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(p, o).expect("write");
}

/// Runs `p` over `x` (mono duplicated to stereo), returns mono output shifted back by latency.
fn run(p: &mut dyn Plugin, x: &[f32]) -> Vec<f32> {
    let lat = p.latency();
    let mut out = Vec::with_capacity(x.len() + lat);
    let mut i = 0;
    let total = x.len() + lat;
    while i < total {
        let n = 512.min(total - i);
        let blk: Vec<f32> = (i..i + n).map(|k| x.get(k).copied().unwrap_or(0.0)).collect();
        let mut io = vec![blk.clone(), blk];
        p.process(&mut io, n);
        out.extend_from_slice(&io[0]);
        i += n;
    }
    eprintln!("  latency {lat} samples");
    out.split_off(lat.min(out.len()))
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (sr, mut x) = read_wav(Path::new(&a[1]));
    let dir = PathBuf::from(&a[2]);
    std::fs::create_dir_all(&dir).unwrap();
    let start: f32 = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let len: f32 = a.get(4).and_then(|s| s.parse().ok()).unwrap_or(1e9);
    let s0 = ((start * sr as f32) as usize).min(x.len());
    let s1 = (s0 + (len * sr as f32) as usize).min(x.len());
    x = x[s0..s1].to_vec();
    write_wav(&dir.join("dry.wav"), sr, &x);

    // Plumb: D minor, flex 0, Normal mode; speeds 0, 20, 50 ms, plus Record.
    for (name, speed, mode) in [("plumb_s0", 0.0, 0.0), ("plumb_s5", 5.0, 0.0), ("plumb_s10", 10.0, 0.0), ("plumb_s20", 20.0, 0.0), ("plumb_s35", 35.0, 0.0), ("plumb_s50", 50.0, 0.0), ("plumb_s75", 75.0, 0.0), ("plumb_s100", 100.0, 0.0), ("plumb_rec", 0.0, 1.0)] {
        eprintln!("{name}");
        let mut t = Tune::new();
        t.prepare(sr as f32, 512, 2);
        t.set_param("key", 2.0);
        t.set_param("scale", 2.0);
        t.set_param("speed", speed);
        t.set_param("flex", 0.0);
        t.set_param("mode", mode);
        eprintln!("  readback key {:?} scale {:?} speed {:?} flex {:?} mode {:?} ref {:?}", t.param("key"), t.param("scale"), t.param("speed"), t.param("flex"), t.param("mode"), t.param("reference"));
        let y = run(&mut t, &x);
        write_wav(&dir.join(format!("{name}.wav")), sr, &y);
    }

    // Xpitch via the host.
    let vdir = PathBuf::from(r"C:\Program Files\Common Files\VST3\Nuro Audio");
    let found = soundcraft_vst3_host::scan_paths(std::slice::from_ref(&vdir));
    let d = found.iter().find(|d| d.name.to_lowercase().contains("xpitch")).expect("xpitch");
    soundcraft_vst3_host::add_search_dir(&vdir);
    for (name, tr) in [("xpitch_t00", 0.0f32), ("xpitch_t05", 0.05), ("xpitch_t10", 0.1), ("xpitch_t15", 0.15), ("xpitch_t20", 0.2), ("xpitch_t30", 0.3), ("xpitch_t40", 0.4), ("xpitch_t50", 0.5), ("xpitch_t100", 1.0)] {
        eprintln!("{name}");
        let mut p = soundcraft_vst3_host::create(&d.id).expect("create");
        p.prepare(sr as f32, 512, 2);
        eprintln!("  set mode {}", p.set_param("11040722", 2.0));
        eprintln!("  set key {}", p.set_param("322477887", 2.0));
        eprintln!("  set scale {}", p.set_param("322547095", 1.0));
        eprintln!("  set trans {}", p.set_param("441993146", tr));
        eprintln!("  readback mode {:?} key {:?} scale {:?} trans {:?}", p.param("11040722"), p.param("322477887"), p.param("322547095"), p.param("441993146"));
        let y = run(p.as_mut(), &x);
        write_wav(&dir.join(format!("{name}.wav")), sr, &y);
    }
}
