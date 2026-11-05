//! Pitch tracks (YIN, 26 ms window, 2.5 ms hop) of WAVs: `ptrack <dir> name:lag_ms ...` prints `name t_sec midi`.
use soundcraft_dsp::pitch_detect::{YinScratch, yin_with};
fn read(p: &str) -> Vec<f32> {
    let b = std::fs::read(p).expect("read");
    b[44..].chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect()
}
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let dir = &a[1];
    let mut sc = YinScratch::default();
    sc.reserve(1248);
    for spec in &a[2..] {
        let (name, lag) = spec.split_once(':').expect("name:lag");
        let lag: f32 = lag.parse().expect("lag");
        let x = read(&format!("{dir}/{name}.wav"));
        let off = (lag * 48.0).round() as usize;
        let mut i = 0usize;
        while i + off + 1248 <= x.len() {
            let fr = &x[i + off..i + off + 1248];
            let rms = (fr.iter().map(|v| v * v).sum::<f32>() / 1248.0).sqrt();
            if rms > 0.003 {
                if let Some(f) = yin_with(fr, 48_000.0, 80.0, 1200.0, 0.15, &mut sc) {
                    println!("{name} {:.4} {:.3}", (i + 624) as f32 / 48_000.0, 69.0 + 12.0 * (f / 440.0).log2());
                }
            }
            i += 120;
        }
    }
}
