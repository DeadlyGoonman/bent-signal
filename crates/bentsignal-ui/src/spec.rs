//! What a plugin tells the shared editor. All positions are on the 640-wide base canvas.

/// The bundle name shown in every plugin's header.
pub const BUNDLE_NAME: &str = "Bent Signal";
/// Width of the base canvas; plugins choose only the height.
pub const BASE_W: i32 = 640;

/// The plugin's parameters as the editor needs them (ids are VST3 ids; the last one is bypass).
pub trait ParamModel: Sync {
    /// Number of parameters, bypass included.
    fn count(&self) -> usize;
    /// Number of DSP parameters (ids below this are saved in presets).
    fn dsp_count(&self) -> usize;
    fn to_plain(&self, id: u32, n: f64) -> f64;
    fn to_normalized(&self, id: u32, v: f64) -> f64;
    fn default_normalized(&self, id: u32) -> f64;
    /// Display text of a normalized value, with unit.
    fn format(&self, id: u32, n: f64) -> String;
    /// Hover text.
    fn tip(&self, id: u32) -> &'static str;
    /// Value text under a knob whose caption is below it (defaults to `format`).
    fn format_short(&self, id: u32, n: f64) -> String {
        self.format(id, n)
    }
    /// Meter marker (dBFS) for the current normalized values, when the plugin has one.
    fn meter_marker_db(&self, _values: &[f64]) -> Option<f32> {
        None
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Preset {
    pub name: &'static str,
    /// Plain values for ids `0..dsp_count`.
    pub values: &'static [f64],
}

/// A rotary knob. Radius above 20 is a "large" knob (caption above); smaller ones put the caption
/// and value beside the dial.
#[derive(Debug, Clone, Copy)]
pub struct KnobSpec {
    pub id: u32,
    pub caption: &'static str,
    pub ends: (&'static str, &'static str),
    pub cx: i32,
    pub cy: i32,
    pub r: i32,
    /// Plain value where the value arc starts (a bipolar knob); `None` starts at the minimum.
    pub origin: Option<f64>,
    /// Half-width of the end-label row under the dial; `None` is the radius plus 22. Tighter
    /// grids set a smaller value so neighbouring labels do not touch.
    pub end_span: Option<i32>,
    /// Caption and value under the dial (no end labels), instead of above or beside it.
    pub below: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    /// The polarity symbol (circle with a slash).
    Phase,
    /// A plain lamp.
    Lamp,
    /// A checkbox with its label to the right (no ON/OFF text).
    Check,
}

/// An on/off switch with a label above and an ON/OFF text below.
#[derive(Debug, Clone, Copy)]
pub struct ToggleSpec {
    pub id: u32,
    pub label: &'static str,
    pub cx: i32,
    pub cy: i32,
    pub glyph: Glyph,
    pub on_text: &'static str,
    pub off_text: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct SelectorOption {
    /// Marking above the button (I, II, 1...).
    pub mark: &'static str,
    /// Name shown below the group while selected.
    pub name: &'static str,
    pub tip: &'static str,
}

/// One row of exclusive buttons backed by a list parameter.
#[derive(Debug, Clone, Copy)]
pub struct SelectorSpec {
    pub id: u32,
    pub caption: &'static str,
    pub cx: i32,
    pub cy: i32,
    /// Distance between button centres.
    pub spacing: i32,
    pub options: &'static [SelectorOption],
}

/// A dropdown list backed by a list parameter: a box showing the current choice that opens a
/// menu of `options` (one per list value).
#[derive(Debug, Clone, Copy)]
pub struct DropdownSpec {
    pub id: u32,
    pub caption: &'static str,
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
    pub options: &'static [&'static str],
}

/// A box showing a number (the parameter's plain value, with its unit) that is edited by typing:
/// click, type, Enter to apply, Esc to cancel.
#[derive(Debug, Clone, Copy)]
pub struct NumberSpec {
    pub id: u32,
    pub caption: &'static str,
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

/// A radio button: lit while the parameter `id` has the plain value `value`; clicking sets it.
#[derive(Debug, Clone, Copy)]
pub struct RadioSpec {
    pub id: u32,
    pub value: f64,
    /// Empty draws a bare dot (no label).
    pub label: &'static str,
    pub cx: i32,
    pub cy: i32,
    /// Radius of the dot.
    pub r: i32,
    pub tip: &'static str,
    /// Plain value a click on the lit radio sets (so it can switch itself off).
    pub off_value: Option<f64>,
}

/// A thin horizontal divider line.
#[derive(Debug, Clone, Copy)]
pub struct RuleSpec {
    pub x0: i32,
    pub x1: i32,
    pub y: i32,
}

/// A fixed text on the canvas.
#[derive(Debug, Clone, Copy)]
pub struct LabelSpec {
    pub text: &'static str,
    pub cx: i32,
    pub cy: i32,
}

/// The look of a plugin's editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    /// Walnut veneer, preset bar, wordmark and the warm accent.
    Walnut,
    /// Plain dark grey panel with lighter grey controls and brighter outlines; no bar or veneer.
    Grey,
}

/// A horizontal input meter with a marker.
#[derive(Debug, Clone, Copy)]
pub struct MeterSpec {
    pub caption: &'static str,
    /// Bar extent and vertical centre.
    pub x0: i32,
    pub x1: i32,
    pub cy: i32,
    pub floor_db: f32,
    pub ceil_db: f32,
    pub tip: &'static str,
}

pub struct UiSpec {
    /// Wordmark in the header, e.g. "Shifter".
    pub title: &'static str,
    /// Window class name (unique per plugin).
    pub class_name: &'static str,
    /// Canvas width (`BASE_W` for the Bent Signal plugins) and height.
    pub base_w: i32,
    pub base_h: i32,
    pub knobs: &'static [KnobSpec],
    pub toggles: &'static [ToggleSpec],
    pub selector: Option<SelectorSpec>,
    pub dropdowns: &'static [DropdownSpec],
    pub numbers: &'static [NumberSpec],
    pub radios: &'static [RadioSpec],
    pub labels: &'static [LabelSpec],
    pub rules: &'static [RuleSpec],
    pub theme: Theme,
    pub meter: Option<MeterSpec>,
    pub bypass_id: u32,
    pub presets: &'static [Preset],
    pub presets_tip: &'static str,
    pub model: &'static dyn ParamModel,
}
