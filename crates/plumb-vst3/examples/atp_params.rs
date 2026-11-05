//! Lists a VST3's parameters as the soundcraft host sees them. Usage: atp_params <vst3 dir> <name substring>
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let dir = std::path::PathBuf::from(&a[1]);
    let found = soundcraft_vst3_host::scan_paths(std::slice::from_ref(&dir));
    soundcraft_vst3_host::add_search_dir(&dir);
    for d in &found {
        println!("found {} {}", d.name, d.id);
    }
    let d = found.iter().find(|d| d.name.to_lowercase().contains(&a[2].to_lowercase())).expect("plugin");
    let mut p = soundcraft_vst3_host::create(&d.id).expect("create");
    p.prepare(48000.0, 512, 2);
    if let Some(mut ed) = p.editor() {
        for id in a.iter().skip(3) {
            let q = p.info().param(id).expect("param");
            let steps: Vec<f32> = if q.max - q.min > 40.0 || q.max <= 1.0 { (0..=20).map(|i| q.min + (q.max - q.min) * i as f32 / 20.0).collect() } else { (0..=(q.max - q.min) as i32).map(|i| q.min + i as f32).collect() };
            for v in steps {
                println!("text {} {} = {:?}", q.name, v, ed.value_text(id, v));
            }
        }
    } else {
        println!("no editor");
    }
    for q in p.info().params {
        println!("{} | {} | def {} | range {}..{} | val {:?}", q.id, q.name, q.default, q.min, q.max, p.param(q.id));
    }
}
