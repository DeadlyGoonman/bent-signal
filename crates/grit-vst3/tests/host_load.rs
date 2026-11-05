//! Loads the built Grit plugin as a real `.vst3` bundle through SoundCraft's own VST3 host
//! (in-process, offline: no audio device) and runs it: scan, instantiate, process, parameters,
//! state. Uses the bundle in `GRIT_VST3_BUNDLE` when set, else bundles the cdylib cargo built next
//! to this test.

use soundcraft_dsp::{Category, Unit};
use std::path::{Path, PathBuf};

/// A folder holding `Grit.vst3`.
fn bundle_dir() -> PathBuf {
    if let Some(b) = std::env::var_os("GRIT_VST3_BUNDLE") {
        let b = PathBuf::from(b);
        return b.parent().map(Path::to_path_buf).unwrap_or(b);
    }
    let exe = std::env::current_exe().unwrap();
    let deps = exe.parent().unwrap();
    let lib = if cfg!(windows) {
        "Grit.dll"
    } else if cfg!(target_os = "macos") {
        "libGrit.dylib"
    } else {
        "libGrit.so"
    };
    let built = [deps.join(lib), deps.parent().unwrap().join(lib)].into_iter().find(|p| p.exists()).expect("the Grit cdylib next to the test");
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("grit-vst3-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (folder, exe_name) = if cfg!(windows) {
        ("x86_64-win", "Grit.vst3")
    } else if cfg!(target_os = "macos") {
        ("MacOS", "Grit")
    } else {
        ("x86_64-linux", "Grit.so")
    };
    let bin = dir.join("Grit.vst3/Contents").join(folder);
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::copy(&built, bin.join(exe_name)).unwrap();
    dir
}

#[test]
fn the_bundle_loads_and_processes_in_the_soundcraft_host() {
    let dir = bundle_dir();
    assert_eq!(soundcraft_vst3_host::add_search_dir(&dir), 1);
    let found = soundcraft_vst3_host::scan_paths(std::slice::from_ref(&dir));
    assert_eq!(found.len(), 1, "{found:?}");
    let d = &found[0];
    assert_eq!(d.name, "Grit");
    assert_eq!(d.vendor, "SoundCraft");
    assert_eq!(d.sub_categories, vec!["Fx", "Distortion"]);
    assert!(!d.is_instrument);
    let _ = Category::Harmonic;

    if let Err(e) = soundcraft_vst3_host::instantiate(&d.id) {
        panic!("instantiate: {e}");
    }
    let info = soundcraft_vst3_host::plugin_info(&d.id).unwrap();
    let ids: Vec<&str> = info.params.iter().map(|p| p.id).collect();
    assert_eq!(ids, ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10"], "bypass (11) is the host's");
    let style = info.param("0").unwrap();
    assert_eq!((style.unit, style.choices), (Unit::Choice, &["Tape", "Germanium", "Console", "Triode", "Pentode"][..]));

    let mut p = soundcraft_vst3_host::create(&d.id).unwrap();
    p.prepare(48_000.0, 512, 2);
    assert_eq!(p.latency(), 0);
    assert!(p.set_param("4", 80.0));
    assert!(p.set_param("0", 3.0));
    assert_eq!(p.param("0"), Some(3.0));
    let mut all = Vec::new();
    for b in 0..40 {
        let x: Vec<f32> = (0..512).map(|i| 0.5 * ((b * 512 + i) as f32 * 0.03).sin()).collect();
        let mut io = vec![x.clone(), x];
        p.process(&mut io, 512);
        all.extend(io.into_iter().flatten());
    }
    assert!(all.iter().all(|v| v.is_finite() && v.abs() < 4.0));
    assert!(all.iter().any(|v| v.abs() > 0.01));

    // State through the host's blob.
    assert!(p.set_param("1", 120.0));
    let blob = p.save_state().unwrap();
    let mut q = soundcraft_vst3_host::create(&d.id).unwrap();
    q.prepare(44_100.0, 256, 2);
    assert!(q.load_state(&blob));
    assert_eq!(q.param("0"), Some(3.0));
    assert!((q.param("1").unwrap() - 120.0).abs() < 1e-2);
}
