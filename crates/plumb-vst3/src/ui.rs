//! Plumb's description for the shared Bent Signal editor, laid out as drawn on a portrait 9:16
//! canvas (360 x 640), top to bottom: the key dropdown with the Major / Minor radios on one row,
//! the big Retune knob, a divider, the small Flex knob beside the Low Lat checkbox, and the Link
//! row of five large dots. Dark grey theme. The A4 reference and the other scales have no control
//! here (they stay on the host's parameter list); the editor draws and handles input in
//! `bentsignal-ui`.

use crate::params::{self, BYPASS, FLEX, KEY, KEY_NAMES, MODE, MODEL, PRESETS, SCALE, SPEED, SYNC};
use bentsignal_ui::{DropdownSpec, Glyph, KnobSpec, LabelSpec, RadioSpec, RuleSpec, Theme, ToggleSpec, UiSpec};

const BASE_W: i32 = 360;
const BASE_H: i32 = 640;

/// Retune is the big dial (its arc starts at 0, so Lock below 0 reads as its own side); Flex is
/// the small one. Both show their value right under the name.
static KNOBS: [KnobSpec; 2] = [
    KnobSpec { id: SPEED, caption: "RETUNE", ends: ("", ""), cx: 180, cy: 170, r: 70, origin: Some(0.0), end_span: None, below: true },
    KnobSpec { id: FLEX, caption: "FLEX", ends: ("", ""), cx: 90, cy: 392, r: 28, origin: None, end_span: None, below: true },
];

static DROPDOWNS: [DropdownSpec; 1] = [DropdownSpec { id: KEY, caption: "KEY", x0: 20, y0: 20, x1: 176, y1: 64, options: &KEY_NAMES }];

const fn dot(value: f64, cx: i32, tip: &'static str) -> RadioSpec {
    RadioSpec { id: SYNC, value, label: "", cx, cy: 596, r: 24, tip, off_value: Some(0.0) }
}

/// Major and Minor set the Scale parameter (same row as the key); the five dots set the Link group
/// (a lit dot unlinks).
static RADIOS: [RadioSpec; 7] = [
    RadioSpec { id: SCALE, value: 1.0, label: "MAJ", cx: 218, cy: 42, r: 10, tip: params::SCALE_TIPS[1], off_value: None },
    RadioSpec { id: SCALE, value: 2.0, label: "MIN", cx: 292, cy: 42, r: 10, tip: params::SCALE_TIPS[2], off_value: None },
    dot(1.0, 44, "Link 1: every Plumb on this dot shares key, scale, retune, flex and mode. Click again to unlink."),
    dot(2.0, 112, "Link 2: every Plumb on this dot shares key, scale, retune, flex and mode. Click again to unlink."),
    dot(3.0, 180, "Link 3: every Plumb on this dot shares key, scale, retune, flex and mode. Click again to unlink."),
    dot(4.0, 248, "Link 4: every Plumb on this dot shares key, scale, retune, flex and mode. Click again to unlink."),
    dot(5.0, 316, "Link 5: every Plumb on this dot shares key, scale, retune, flex and mode. Click again to unlink."),
];

static LABELS: [LabelSpec; 1] = [LabelSpec { text: "LINK", cx: 180, cy: 532 }];

static RULES: [RuleSpec; 1] = [RuleSpec { x0: 20, x1: 340, y: 318 }];

/// Low Lat is the Mode list shown as a checkbox: checked is Record.
static TOGGLES: [ToggleSpec; 1] =
    [ToggleSpec { id: MODE, label: "LOW LAT", cx: 232, cy: 392, glyph: Glyph::Check, on_text: "", off_text: "" }];

pub static SPEC: UiSpec = UiSpec {
    title: "Plumb",
    class_name: "BentSignalPlumbEditor",
    base_w: BASE_W,
    base_h: BASE_H,
    knobs: &KNOBS,
    toggles: &TOGGLES,
    selector: None,
    dropdowns: &DROPDOWNS,
    radios: &RADIOS,
    labels: &LABELS,
    rules: &RULES,
    theme: Theme::Grey,
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
    fn layout_is_clean() {
        layout::assert_layout_ok(&SPEC);
        let l = layout::layout(&SPEC, BASE_W, BASE_H);
        assert_eq!(l.toggles.len(), 1);
        assert_eq!(l.knobs.len(), 2);
        assert_eq!((l.dropdowns.len(), l.radios.len(), l.labels.len(), l.rules.len()), (1, 7, 1, 1));
        assert!(l.selector.is_none());
        assert_eq!(layout::default_size(&SPEC), (360, 640), "9:16");
        assert_eq!(layout::min_size(&SPEC), (270, 480));
    }

    #[test]
    fn every_control_reaches_its_parameter() {
        let mut ids: Vec<u32> = SPEC.knobs.iter().map(|k| k.id).collect();
        ids.extend(SPEC.toggles.iter().map(|t| t.id));
        ids.extend(SPEC.dropdowns.iter().map(|d| d.id));
        ids.extend(SPEC.radios.iter().map(|r| r.id));
        ids.sort_unstable();
        ids.dedup();
        // Everything but the A4 reference (id 4), which stays on the host's parameter list.
        assert_eq!(ids, [KEY, SCALE, SPEED, FLEX, MODE, SYNC]);
        // Major and Minor are scale values 1 and 2; the dots are the groups 1..=5.
        assert_eq!(soundcraft_dsp::plugins::tune::SCALES[1], "Major");
        assert_eq!(soundcraft_dsp::plugins::tune::SCALES[2], "Minor");
        let groups: Vec<f64> = SPEC.radios.iter().filter(|r| r.id == SYNC).map(|r| r.value).collect();
        assert_eq!(groups, [1.0, 2.0, 3.0, 4.0, 5.0]);
    }

    /// The order and alignment are the drawing's: key, Major and Minor on one row; Retune big and
    /// centred; a divider; Flex and Low Lat on one row; Link and its dots, all on one row.
    #[test]
    fn controls_sit_where_they_were_drawn() {
        let l = layout::layout(&SPEC, BASE_W, BASE_H);
        let (retune, flex) = (&l.knobs[0], &l.knobs[1]);
        let (key, maj, min) = (l.dropdowns[0].rect, l.radios[0].dot, l.radios[1].dot);
        assert_eq!(maj.1, min.1, "Major and Minor on one row");
        assert_eq!((key.top + key.bottom) / 2, maj.1, "level with the key");
        assert!(key.right < maj.0 && maj.0 < min.0, "key, Major, Minor left to right");
        assert_eq!(retune.cx, BASE_W / 2, "Retune is centred");
        assert!(retune.cy - retune.r > key.bottom, "Retune is under the top row");
        assert!(retune.r > flex.r * 2, "Retune is the big one");
        let rule = l.rules[0];
        assert!(retune.value.bottom < rule.top && rule.bottom < flex.cy - flex.r, "the divider is between them");
        let low_lat = l.toggles[0].rect;
        assert_eq!(flex.cy, (low_lat.top + low_lat.bottom) / 2, "Flex and Low Lat on one row");
        assert!(flex.cx < low_lat.left, "Flex left, Low Lat right");
        let dots: Vec<_> = l.radios[2..].iter().map(|r| r.dot).collect();
        assert!(dots.iter().all(|d| d.1 == dots[0].1), "the dots on one row");
        assert!(dots.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(dots[0].1 > l.labels[0].0.bottom, "the dots are under the Link label");
        assert!(l.radios[2].hit.height() >= 2 * 24 - 2, "large dots");
    }

    #[cfg(windows)]
    #[test]
    fn editor_renders_headless() {
        use std::sync::Arc;
        let shared = Arc::new(params::new_shared());
        shared.set(KEY, params::to_normalized(KEY, 2.0));
        shared.set(SCALE, params::to_normalized(SCALE, 2.0));
        shared.set(SPEED, params::to_normalized(SPEED, 150.0));
        shared.set(FLEX, params::to_normalized(FLEX, 25.0));
        shared.set(MODE, 1.0);
        shared.set(SYNC, params::to_normalized(SYNC, 2.0));
        let path = std::env::var_os("PLUMB_UI_PNG").map(std::path::PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("plumb_ui.png"));
        let r = bentsignal_ui::editor::capture_png(shared, &SPEC, None, &path);
        assert_eq!(r, Ok(()));
        eprintln!("editor capture: {}", path.display());
    }

    #[cfg(windows)]
    #[test]
    fn the_editor_reopens_at_the_size_it_was_left_at() {
        use std::sync::Arc;
        let shared = Arc::new(params::new_shared());
        assert_eq!(shared.saved_size(), None);
        shared.save_size(450, 800);
        assert_eq!(shared.saved_size(), Some((450, 800)));
        // The state round trip keeps it too.
        let blob = params::encode_state(&shared.snapshot(), shared.saved_size());
        assert_eq!(params::decode_state(&blob).and_then(|d| d.size), Some((450, 800)));
    }

    #[cfg(windows)]
    #[test]
    fn presets_match() {
        let shared = params::new_shared();
        let m = |user: Option<&Vec<f64>>| bentsignal_ui::editor::matching_preset(&SPEC, &shared.snapshot(), user);
        assert_eq!(m(None), Some(0), "defaults are the Default preset");
        shared.set(FLEX, 0.9);
        assert_eq!(m(None), None);
        let user = shared.snapshot();
        assert_eq!(m(Some(&user)), Some(PRESETS.len()));
    }
}
