//! Shifter's description for the shared Bent Signal editor: which knobs, the mode selector and
//! the phase switch sit where. Everything else (drawing, input, chrome) is `bentsignal-ui`.

use crate::params::{self, BYPASS, MODEL, PHASE_SWAP, PRESETS};
use bentsignal_ui::{Glyph, KnobSpec, SelectorOption, SelectorSpec, ToggleSpec, UiSpec};

const fn knob(id: u32, caption: &'static str, lo: &'static str, hi: &'static str, cx: i32, cy: i32, r: i32, origin: Option<f64>) -> KnobSpec {
    KnobSpec { id, caption, ends: (lo, hi), cx, cy, r, origin, end_span: None, below: false }
}

/// Mix, Anchor, Detune, Delay (large), Width, Output (small). Output is bipolar around 0 dB.
static KNOBS: [KnobSpec; 6] = [
    knob(5, "MIX", "DRY", "WET", 80, 150, 30, None),
    knob(3, "ANCHOR", "20 HZ", "1 KHZ", 200, 150, 30, None),
    knob(2, "DETUNE", "MIN", "MAX", 440, 150, 30, None),
    knob(1, "DELAY", "TIGHT", "LOOSE", 560, 150, 30, None),
    knob(4, "WIDTH", "MONO", "WIDE", 140, 270, 17, None),
    knob(6, "OUTPUT", "-24", "+12", 500, 270, 17, Some(0.0)),
];

static TOGGLES: [ToggleSpec; 1] = [ToggleSpec { id: PHASE_SWAP, label: "PHASE SWAP", cx: 320, cy: 269, glyph: Glyph::Phase, on_text: "ON", off_text: "OFF" }];

static MODES: [SelectorOption; 3] = [
    SelectorOption { mark: "I", name: "Twin", tip: params::MODE_TIPS[0] },
    SelectorOption { mark: "II", name: "Drift", tip: params::MODE_TIPS[1] },
    SelectorOption { mark: "III", name: "Spread", tip: params::MODE_TIPS[2] },
];

pub static SPEC: UiSpec = UiSpec {
    title: "Shifter",
    class_name: "BentSignalShifterEditor",
    base_w: bentsignal_ui::BASE_W,
    base_h: 330,
    knobs: &KNOBS,
    toggles: &TOGGLES,
    selector: Some(SelectorSpec { id: params::MODE, caption: "MODE", cx: 320, cy: 150, spacing: 50, options: &MODES }),
    dropdowns: &[],
    radios: &[],
    labels: &[],
    rules: &[],
    theme: bentsignal_ui::Theme::Walnut,
    meter: None,
    bypass_id: BYPASS,
    presets: &PRESETS,
    presets_tip: params::PRESETS_TIP,
    model: &MODEL,
};

#[cfg(test)]
mod tests {
    use super::*;
    use bentsignal_ui::layout;

    #[test]
    fn layout_is_clean_and_matches_the_house_grid() {
        layout::assert_layout_ok(&SPEC);
        let l = layout::layout(&SPEC, 640, 330);
        assert_eq!(l.knobs.iter().map(|k| k.id).collect::<Vec<_>>(), [5, 3, 2, 1, 4, 6]);
        assert_eq!(l.selector.as_ref().map(|s| s.buttons.len()), Some(3));
        assert_eq!(l.knobs[5].origin, params::to_normalized(6, 0.0));
        assert!(layout::tooltip(&SPEC, layout::Hit::Option(2)).contains("mono"));
        assert_eq!(layout::default_size(&SPEC), (640, 330));
        assert_eq!(layout::min_size(&SPEC), (480, 248));
        assert_eq!(layout::max_size(&SPEC), (1920, 990));
    }

    #[test]
    fn every_parameter_has_a_tip_and_knobs_cover_the_dsp() {
        for id in 0..params::PARAM_COUNT as u32 {
            assert!(!params::tip(id).is_empty());
        }
        let mut ids: Vec<u32> = SPEC.knobs.iter().map(|k| k.id).collect();
        ids.push(params::MODE);
        ids.push(PHASE_SWAP);
        ids.sort_unstable();
        assert_eq!(ids, (0..8).collect::<Vec<_>>());
    }

    #[cfg(windows)]
    #[test]
    fn editor_renders_headless() {
        use crate::params::{MODE, new_shared};
        use std::sync::Arc;
        let shared = Arc::new(new_shared());
        shared.set(MODE, 0.5);
        shared.set(PHASE_SWAP, 1.0);
        let path = std::env::var_os("SHIFTER_UI_PNG").map(std::path::PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("shifter_ui.png"));
        let r = bentsignal_ui::editor::capture_png(shared, &SPEC, Some(layout::Hit::Option(1)), &path);
        assert_eq!(r, Ok(()));
        eprintln!("editor capture: {}", path.display());
    }

    #[cfg(windows)]
    #[test]
    fn presets_match_and_values_apply() {
        use crate::params::new_shared;
        let shared = new_shared();
        let m = |s: &bentsignal_ui::Shared, user: Option<&Vec<f64>>| bentsignal_ui::editor::matching_preset(&SPEC, &s.snapshot(), user);
        assert_eq!(m(&shared, None), Some(0), "defaults are the Default preset");
        shared.set(1, 0.9);
        assert_eq!(m(&shared, None), None);
        let user = shared.snapshot();
        assert_eq!(m(&shared, Some(&user)), Some(PRESETS.len()));
    }
}
