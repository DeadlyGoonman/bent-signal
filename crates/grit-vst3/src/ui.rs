//! Grit's description for the shared Bent Signal editor: six large knobs in signal order, four
//! switches under the knobs they belong to, the input meter and the style selector. Everything
//! else (drawing, input, chrome) is `bentsignal-ui`.

use crate::params::{self, AUTO, BYPASS, DRIVE, HIGH_CUT, LOW_CUT, MIX, MODEL, OUTPUT, PRESETS, PUNISH, STEEP, STYLE, THUMP, TONE};
use bentsignal_ui::{Glyph, KnobSpec, MeterSpec, SelectorOption, SelectorSpec, ToggleSpec, UiSpec};

const fn knob(id: u32, caption: &'static str, lo: &'static str, hi: &'static str, cx: i32, origin: Option<f64>) -> KnobSpec {
    KnobSpec { id, caption, ends: (lo, hi), cx, cy: 130, r: 30, origin, end_span: Some(40), below: false }
}

const fn switch(id: u32, label: &'static str, cx: i32) -> ToggleSpec {
    ToggleSpec { id, label, cx, cy: 240, glyph: Glyph::Lamp, on_text: "ON", off_text: "OFF" }
}

/// Signal order, left to right on a 102 px pitch. Tone and Output are bipolar around their centre.
static KNOBS: [KnobSpec; 6] = [
    knob(LOW_CUT, "LOW CUT", "OFF", "400 HZ", 64, None),
    knob(TONE, "TONE", "DARK", "BRIGHT", 166, Some(0.0)),
    knob(DRIVE, "DRIVE", "CLEAN", "HOT", 268, None),
    knob(HIGH_CUT, "HIGH CUT", "1 KHZ", "OFF", 370, None),
    knob(OUTPUT, "OUTPUT", "-24", "+12", 472, Some(0.0)),
    knob(MIX, "MIX", "DRY", "WET", 574, None),
];

/// Each switch sits under the knob it belongs to.
static TOGGLES: [ToggleSpec; 4] = [switch(THUMP, "THUMP", 64), switch(PUNISH, "PUNISH +20", 268), switch(STEEP, "STEEP", 370), switch(AUTO, "AUTO GAIN", 472)];

static STYLES: [SelectorOption; 5] = [
    SelectorOption { mark: "I", name: "Tape", tip: params::STYLE_TIPS[0] },
    SelectorOption { mark: "II", name: "Germanium", tip: params::STYLE_TIPS[1] },
    SelectorOption { mark: "III", name: "Console", tip: params::STYLE_TIPS[2] },
    SelectorOption { mark: "IV", name: "Triode", tip: params::STYLE_TIPS[3] },
    SelectorOption { mark: "V", name: "Pentode", tip: params::STYLE_TIPS[4] },
];

pub static SPEC: UiSpec = UiSpec {
    title: "Grit",
    class_name: "BentSignalGritEditor",
    base_w: bentsignal_ui::BASE_W,
    base_h: 420,
    knobs: &KNOBS,
    toggles: &TOGGLES,
    selector: Some(SelectorSpec { id: STYLE, caption: "STYLE", cx: 320, cy: 350, spacing: 40, options: &STYLES }),
    dropdowns: &[],
    radios: &[],
    labels: &[],
    rules: &[],
    theme: bentsignal_ui::Theme::Walnut,
    meter: Some(MeterSpec { caption: "INPUT", x0: 70, x1: 570, cy: 292, floor_db: -48.0, ceil_db: 0.0, tip: params::METER_TIP }),
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
    fn layout_is_clean() {
        layout::assert_layout_ok(&SPEC);
        let l = layout::layout(&SPEC, 640, 420);
        assert_eq!(l.knobs.len(), 6);
        assert_eq!(l.toggles.len(), 4);
        assert_eq!(l.selector.as_ref().map(|s| s.buttons.len()), Some(5));
        assert_eq!(layout::min_size(&SPEC), (480, 315));
    }

    #[test]
    fn every_parameter_has_a_control() {
        let mut ids: Vec<u32> = SPEC.knobs.iter().map(|k| k.id).collect();
        ids.extend(SPEC.toggles.iter().map(|t| t.id));
        ids.extend(SPEC.selector.map(|s| s.id));
        ids.sort_unstable();
        assert_eq!(ids, (0..params::DSP_COUNT as u32).collect::<Vec<_>>());
    }

    #[cfg(windows)]
    #[test]
    fn editor_renders_headless() {
        use std::sync::Arc;
        let shared = Arc::new(params::new_shared());
        shared.set(STYLE, 0.75);
        shared.set(PUNISH, 1.0);
        shared.set(AUTO, 1.0);
        shared.set(TONE, 0.7);
        shared.set(DRIVE, 0.55);
        shared.set(LOW_CUT, 0.3);
        shared.set(THUMP, 1.0);
        shared.set_meter(0.12);
        let path = std::env::var_os("GRIT_UI_PNG").map(std::path::PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("grit_ui.png"));
        let r = bentsignal_ui::editor::capture_png(shared, &SPEC, Some(layout::Hit::Meter), &path);
        assert_eq!(r, Ok(()));
        eprintln!("editor capture: {}", path.display());
    }

    #[cfg(windows)]
    #[test]
    fn presets_match() {
        let shared = params::new_shared();
        let m = |user: Option<&Vec<f64>>| bentsignal_ui::editor::matching_preset(&SPEC, &shared.snapshot(), user);
        assert_eq!(m(None), Some(0), "defaults are the Default preset");
        shared.set(DRIVE, 0.9);
        assert_eq!(m(None), None);
        let user = shared.snapshot();
        assert_eq!(m(Some(&user)), Some(PRESETS.len()));
    }
}
