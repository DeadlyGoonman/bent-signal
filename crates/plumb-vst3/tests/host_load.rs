//! Loads the built Plumb plugin as a real `.vst3` bundle through SoundCraft's own VST3 host
//! (in-process, offline: no audio device) and runs it: scan, instantiate, process, parameters,
//! state. Uses the bundle in `PLUMB_VST3_BUNDLE` when set, else bundles the cdylib cargo built next
//! to this test.

use soundcraft_dsp::{Category, Unit};
use std::path::{Path, PathBuf};

/// A folder holding `Plumb.vst3`.
fn bundle_dir() -> PathBuf {
    if let Some(b) = std::env::var_os("PLUMB_VST3_BUNDLE") {
        let b = PathBuf::from(b);
        return b.parent().map(Path::to_path_buf).unwrap_or(b);
    }
    let exe = std::env::current_exe().unwrap();
    let deps = exe.parent().unwrap();
    let lib = if cfg!(windows) {
        "Plumb.dll"
    } else if cfg!(target_os = "macos") {
        "libPlumb.dylib"
    } else {
        "libPlumb.so"
    };
    let built = [deps.join(lib), deps.parent().unwrap().join(lib)].into_iter().find(|p| p.exists()).expect("the Plumb cdylib next to the test");
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("plumb-vst3-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (folder, exe_name) = if cfg!(windows) {
        ("x86_64-win", "Plumb.vst3")
    } else if cfg!(target_os = "macos") {
        ("MacOS", "Plumb")
    } else {
        ("x86_64-linux", "Plumb.so")
    };
    let bin = dir.join("Plumb.vst3/Contents").join(folder);
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
    assert_eq!(d.name, "Plumb");
    assert_eq!(d.vendor, "SoundCraft");
    assert_eq!(d.sub_categories, vec!["Fx", "Pitch Shift"]);
    assert!(!d.is_instrument);
    let _ = Category::PitchShift;

    if let Err(e) = soundcraft_vst3_host::instantiate(&d.id) {
        panic!("instantiate: {e}");
    }
    let info = soundcraft_vst3_host::plugin_info(&d.id).unwrap();
    let ids: Vec<&str> = info.params.iter().map(|p| p.id).collect();
    assert_eq!(ids, ["0", "1", "2", "3", "4", "5", "6", "8"], "bypass (7) is the host's");
    assert_eq!(info.param("8").unwrap().choices.len(), 6, "Link: Off and five groups");
    let key = info.param("0").unwrap();
    assert_eq!((key.unit, key.choices.len()), (Unit::Choice, 12));
    assert_eq!(info.param("1").unwrap().choices.len(), 13);

    let mut p = soundcraft_vst3_host::create(&d.id).unwrap();
    p.prepare(48_000.0, 512, 2);
    assert_eq!(p.latency(), 960);
    assert!(p.set_param("3", 40.0));
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

    // Link: two instances on group 3 follow each other, one on group 1 and one Off do not.
    let mut a = soundcraft_vst3_host::create(&d.id).unwrap();
    let mut b = soundcraft_vst3_host::create(&d.id).unwrap();
    let mut other = soundcraft_vst3_host::create(&d.id).unwrap();
    let mut off = soundcraft_vst3_host::create(&d.id).unwrap();
    for p in [&mut a, &mut b, &mut other, &mut off] {
        p.prepare(48_000.0, 256, 2);
    }
    // This host sends parameter changes through the processor's queue (applied by `process`).
    let block = |p: &mut Box<dyn soundcraft_dsp::Plugin>| {
        let mut io = vec![vec![0.3f32; 256], vec![0.3f32; 256]];
        p.process(&mut io, 256);
        assert!(io[0].iter().all(|v| v.is_finite()));
    };
    let value = |p: &mut Box<dyn soundcraft_dsp::Plugin>, id: u32| {
        // The host wraps the plugin's own state; find ours by its magic.
        let blob = p.save_state().unwrap();
        let at = blob.windows(4).position(|w| w == b"PLMB").unwrap();
        let d = Plumb::params::decode_state(&blob[at..]).unwrap();
        Plumb::params::to_plain(id, d.values[id as usize])
    };
    assert!(a.set_param("8", 3.0) && b.set_param("8", 3.0) && other.set_param("8", 1.0));
    for p in [&mut a, &mut b, &mut other, &mut off] {
        block(p);
    }
    assert!(a.set_param("0", 5.0));
    assert!(a.set_param("3", 60.0));
    block(&mut a);
    let near = |a: f64, b: f64| (a - b).abs() < 1e-3;
    assert!(near(value(&mut b, 0), 5.0) && near(value(&mut b, 3), 60.0), "same group follows");
    block(&mut b);
    assert_eq!(value(&mut other, 0), 0.0, "another group is untouched");
    assert_eq!(value(&mut off, 0), 0.0, "Off is untouched");
    // A late joiner takes the group's values.
    assert!(off.set_param("8", 3.0));
    block(&mut off);
    assert!(near(value(&mut off, 0), 5.0) && near(value(&mut off, 3), 60.0));

    // State through the host's blob.
    assert!(p.set_param("4", 432.0));
    let blob = p.save_state().unwrap();
    let mut q = soundcraft_vst3_host::create(&d.id).unwrap();
    q.prepare(44_100.0, 256, 2);
    assert!(q.load_state(&blob));
    assert_eq!(q.param("0"), Some(3.0));
    assert!((q.param("4").unwrap() - 432.0).abs() < 1e-2);
}
