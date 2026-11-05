//! Xpitch: does the mode live in the state blob, and does loading that state change the delay?
use std::path::PathBuf;
fn click_delay(p: &mut Box<dyn soundcraft_dsp::Plugin>) -> Option<i64> {
    let total = 24_000usize;
    let x: Vec<f32> = (0..total).map(|i| if i == 9_600 { 0.8 } else { 0.0 }).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < total {
        let n = 256.min(total - i);
        let mut io = vec![x[i..i + n].to_vec(), x[i..i + n].to_vec()];
        p.process(&mut io, n);
        out.extend_from_slice(&io[0]);
        i += n;
    }
    out.iter().enumerate().skip(9_000).position(|(_, v)| v.abs() > 0.05).map(|v| v as i64 + 9_000 - 9_600)
}
fn main() {
    let vdir = PathBuf::from(r"C:\Program Files\Common Files\VST3\Nuro Audio");
    let found = soundcraft_vst3_host::scan_paths(std::slice::from_ref(&vdir));
    let d = found.iter().find(|d| d.name.to_lowercase().contains("xpitch")).expect("xpitch");
    soundcraft_vst3_host::add_search_dir(&vdir);
    let mut blobs = Vec::new();
    for mode in [0.0f32, 1.0, 2.0] {
        let mut p = soundcraft_vst3_host::create(&d.id).expect("create");
        p.prepare(48_000.0, 256, 2);
        p.set_param("11040722", mode);
        let b = p.save_state().expect("state");
        eprintln!("mode {mode}: state len {}", b.len());
        blobs.push(b);
    }
    for i in 1..blobs.len() {
        let diff: Vec<usize> = (0..blobs[0].len().min(blobs[i].len())).filter(|&k| blobs[0][k] != blobs[i][k]).take(10).collect();
        eprintln!("blob0 vs blob{i}: first diff byte offsets {:?}", diff);
    }
    for (i, b) in blobs.iter().enumerate() {
        let mut p = soundcraft_vst3_host::create(&d.id).expect("create");
        let ok = p.load_state(b);
        p.prepare(48_000.0, 256, 2);
        eprintln!("load_state(blob{i}) ok={ok}: mode readback {:?} click_delay {:?} samples", p.param("11040722"), click_delay(&mut p));
    }
}
