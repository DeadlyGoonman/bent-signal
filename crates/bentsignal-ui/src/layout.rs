//! The editor's layout: platform-free rectangles, hit testing, knob geometry and hover texts.
//! Everything is designed on a 640-wide base canvas (the plugin picks the height) and scaled
//! uniformly (centred) to the client size. The Win32 window that draws it lives in `editor`.

use crate::spec::UiSpec;

/// Knob sweep: 270 degrees, from 225° (bottom left, value 0) clockwise to -45° (bottom right).
pub const SWEEP_START_DEG: f64 = 225.0;
pub const SWEEP_DEG: f64 = 270.0;
/// Pixels of vertical mouse travel (at scale 1) for the full knob range.
pub const DRAG_SPAN: f64 = 220.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect { left, top, right, bottom }
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
    pub fn width(&self) -> i32 {
        self.right - self.left
    }
    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }
    pub fn center(&self) -> (i32, i32) {
        ((self.left + self.right) / 2, (self.top + self.bottom) / 2)
    }
}

/// Editor size limits and default, in pixels, for a spec: the default is the base canvas, the
/// minimum three quarters of it, the maximum three times.
pub fn default_size(spec: &UiSpec) -> (i32, i32) {
    (spec.base_w, spec.base_h)
}

pub fn min_size(spec: &UiSpec) -> (i32, i32) {
    ((f64::from(spec.base_w) * 0.75).round() as i32, (f64::from(spec.base_h) * 0.75).round() as i32)
}

pub fn max_size(spec: &UiSpec) -> (i32, i32) {
    (spec.base_w * 3, spec.base_h * 3)
}

pub fn clamp_size(spec: &UiSpec, w: i32, h: i32) -> (i32, i32) {
    let (min_w, min_h) = min_size(spec);
    let (max_w, max_h) = max_size(spec);
    (w.clamp(min_w, max_w), h.clamp(min_h, max_h))
}

/// One knob: caption above (large) or beside (small), dial, end labels, value readout.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Knob {
    pub id: u32,
    pub caption: &'static str,
    pub cx: i32,
    pub cy: i32,
    pub r: i32,
    pub big: bool,
    /// Caption and value under the dial.
    pub below: bool,
    pub label: Rect,
    pub ends: (&'static str, &'static str),
    pub end_left: Rect,
    pub end_right: Rect,
    pub value: Rect,
    /// Where a click grabs the knob.
    pub hit: Rect,
    /// Normalized value where the value arc starts.
    pub origin: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Toggle {
    pub id: u32,
    pub label: Rect,
    pub rect: Rect,
    pub value: Rect,
    /// Where a click toggles it (the box, plus the label of a checkbox).
    pub hit: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dropdown {
    pub id: u32,
    pub rect: Rect,
    pub caption: Rect,
    pub value: Rect,
    pub arrow: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumberBox {
    pub id: u32,
    pub rect: Rect,
    pub caption: Rect,
    pub value: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Radio {
    pub id: u32,
    pub value: f64,
    pub dot: (i32, i32),
    pub label: Rect,
    pub hit: Rect,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Selector {
    pub id: u32,
    pub buttons: Vec<Rect>,
    pub marks: Vec<Rect>,
    pub caption: Rect,
    pub name: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Meter {
    pub caption: Rect,
    pub bar: Rect,
    pub readout: Rect,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub width: i32,
    pub height: i32,
    pub scale: f64,
    /// The preset bar across the top (full width).
    pub bar: Rect,
    pub prev: Rect,
    pub next: Rect,
    pub name: Rect,
    pub save: Rect,
    pub reset: Rect,
    pub bypass: Rect,
    /// The bundle name in the preset bar.
    pub bundle: Rect,
    /// The wordmark and the accent line through the title row.
    pub wordmark: Rect,
    pub line_y: i32,
    pub line_left: (i32, i32),
    pub line_right: (i32, i32),
    pub knobs: Vec<Knob>,
    pub toggles: Vec<Toggle>,
    pub dropdowns: Vec<Dropdown>,
    pub numbers: Vec<NumberBox>,
    pub radios: Vec<Radio>,
    pub selector: Option<Selector>,
    pub labels: Vec<(Rect, &'static str)>,
    pub rules: Vec<Rect>,
    pub meter: Option<Meter>,
    pub grip: Rect,
}

/// What is under the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    PrevPreset,
    NextPreset,
    PresetName,
    Save,
    Reset,
    Bypass,
    /// A selector button, by index.
    Option(usize),
    /// A switch, by index in `UiSpec::toggles`.
    Toggle(usize),
    /// A dropdown, by index in `UiSpec::dropdowns`.
    Dropdown(usize),
    /// A number box, by index in `UiSpec::numbers`.
    Number(usize),
    /// A radio button, by index in `UiSpec::radios`.
    Radio(usize),
    Knob(u32),
    Meter,
    Grip,
}

pub fn layout(spec: &UiSpec, w: i32, h: i32) -> Layout {
    let (w, h) = (w.max(1), h.max(1));
    let (bw, bh) = (f64::from(spec.base_w), f64::from(spec.base_h));
    let s = (f64::from(w) / bw).min(f64::from(h) / bh);
    let ox = (f64::from(w) - bw * s) / 2.0;
    let oy = (f64::from(h) - bh * s) / 2.0;
    let x = |v: i32| (ox + f64::from(v) * s).round() as i32;
    let y = |v: i32| (oy + f64::from(v) * s).round() as i32;
    let d = |v: i32| (f64::from(v) * s).round() as i32;
    let r = |l: i32, t: i32, rr: i32, b: i32| Rect::new(x(l), y(t), x(rr), y(b));
    let model = spec.model;
    let knobs = spec
        .knobs
        .iter()
        .map(|k| {
            let (cx, cy, rad) = (k.cx, k.cy, k.r);
            let big = rad > 20;
            let below = k.below;
            let (label, ends_y, value) = if below {
                (r(cx - 70, cy + rad + 8, cx + 70, cy + rad + 26), cy, r(cx - 70, cy + rad + 27, cx + 70, cy + rad + 43))
            } else if big {
                (r(cx - 60, cy - 58, cx + 60, cy - 40), cy + 34, r(cx - 60, cy + 50, cx + 60, cy + 66))
            } else {
                (r(cx - 90, cy - 9, cx - rad - 10, cy + 9), cy + rad + 1, r(cx + rad + 10, cy - 9, cx + 90, cy + 9))
            };
            let span = k.end_span.unwrap_or(rad + 22);
            Knob {
                id: k.id,
                caption: k.caption,
                cx: x(cx),
                cy: y(cy),
                r: d(rad),
                big,
                below,
                label,
                ends: k.ends,
                end_left: if below { Rect::default() } else { r(cx - span, ends_y, cx, ends_y + 14) },
                end_right: if below { Rect::default() } else { r(cx, ends_y, cx + span, ends_y + 14) },
                value,
                hit: r(cx - rad - 8, cy - rad - 8, cx + rad + 8, cy + rad + 8),
                origin: k.origin.map_or(0.0, |v| model.to_normalized(k.id, v)),
            }
        })
        .collect();
    let toggles = spec
        .toggles
        .iter()
        .map(|t| {
            let rect = r(t.cx - 11, t.cy - 11, t.cx + 11, t.cy + 11);
            if t.glyph == crate::spec::Glyph::Check {
                let box_ = r(t.cx - 14, t.cy - 14, t.cx + 14, t.cy + 14);
                Toggle { id: t.id, label: r(t.cx + 22, t.cy - 12, t.cx + 100, t.cy + 12), rect: box_, value: Rect::default(), hit: r(t.cx - 18, t.cy - 18, t.cx + 104, t.cy + 18) }
            } else {
                Toggle { id: t.id, label: r(t.cx - 60, t.cy - 31, t.cx + 60, t.cy - 15), rect, value: r(t.cx - 60, t.cy + 15, t.cx + 60, t.cy + 29), hit: rect }
            }
        })
        .collect();
    let dropdowns = spec
        .dropdowns
        .iter()
        .map(|d| Dropdown {
            id: d.id,
            rect: r(d.x0, d.y0, d.x1, d.y1),
            caption: r(d.x0 + 10, d.y0, d.x0 + 52, d.y1),
            value: r(d.x0 + 52, d.y0, d.x1 - 26, d.y1),
            arrow: r(d.x1 - 26, d.y0, d.x1 - 6, d.y1),
        })
        .collect();
    let numbers = spec
        .numbers
        .iter()
        .map(|n| NumberBox { id: n.id, rect: r(n.x0, n.y0, n.x1, n.y1), caption: r(n.x0, n.y0 + 3, n.x1, n.y0 + 19), value: r(n.x0, n.y0 + 19, n.x1, n.y1 - 3) })
        .collect();
    let radios = spec
        .radios
        .iter()
        .map(|q| {
            let (rad, left) = (q.r + 3, q.cx + q.r + 6);
            let hit = if q.label.is_empty() { r(q.cx - rad, q.cy - rad, q.cx + rad, q.cy + rad) } else { r(q.cx - rad, q.cy - rad, left + 44, q.cy + rad) };
            Radio { id: q.id, value: q.value, dot: (x(q.cx), y(q.cy)), label: r(left, q.cy - 12, left + 44, q.cy + 12), hit }
        })
        .collect();
    let build_selector = |sel: crate::spec::SelectorSpec| {
        let n = sel.options.len() as i32;
        let centers: Vec<i32> = (0..n).map(|i| sel.cx + (2 * i - (n - 1)) * sel.spacing / 2).collect();
        Selector {
            id: sel.id,
            buttons: centers.iter().map(|&c| r(c - 15, sel.cy - 15, c + 15, sel.cy + 15)).collect(),
            marks: centers.iter().map(|&c| r(c - 20, sel.cy - 35, c + 20, sel.cy - 18)).collect(),
            caption: r(sel.cx - 58, sel.cy + 20, sel.cx + 58, sel.cy + 34),
            name: r(sel.cx - 58, sel.cy + 36, sel.cx + 58, sel.cy + 52),
        }
    };
    let selector = spec.selector.map(build_selector);
    let labels = spec.labels.iter().map(|t| (r(t.cx - 90, t.cy - 11, t.cx + 90, t.cy + 11), t.text)).collect();
    let rules = spec.rules.iter().map(|q| r(q.x0, q.y, q.x1, q.y + 1)).collect();
    let grey = spec.theme == crate::spec::Theme::Grey;
    let off = |rc: Rect| if grey { Rect::default() } else { rc };
    let meter = spec.meter.map(|m| Meter {
        caption: r(m.x0 - 64, m.cy - 7, m.x0 - 8, m.cy + 7),
        bar: r(m.x0, m.cy - 5, m.x1, m.cy + 5),
        readout: r(m.x1 + 8, m.cy - 7, m.x1 + 64, m.cy + 7),
    });
    Layout {
        width: w,
        height: h,
        scale: s,
        bar: Rect::new(0, 0, w, y(32)),
        prev: off(r(10, 6, 30, 26)),
        next: off(r(34, 6, 54, 26)),
        name: off(r(60, 5, 300, 27)),
        save: off(r(306, 6, 326, 26)),
        reset: off(r(330, 6, 350, 26)),
        bypass: off(r(560, 6, 630, 26)),
        bundle: r(380, 6, 550, 26),
        wordmark: r(240, 38, 400, 70),
        line_y: y(54),
        line_left: (x(20), x(236)),
        line_right: (x(404), x(620)),
        knobs,
        toggles,
        dropdowns,
        numbers,
        radios,
        selector,
        labels,
        rules,
        meter,
        grip: Rect::new(w - 18, h - 18, w, h),
    }
}

impl Layout {
    /// Every rectangle that reacts to the mouse, except the grip.
    pub fn interactive(&self) -> Vec<Rect> {
        let mut v = vec![self.prev, self.next, self.name, self.save, self.reset, self.bypass];
        v.extend(self.toggles.iter().map(|t| t.hit));
        v.extend(self.dropdowns.iter().map(|d| d.rect));
        v.extend(self.numbers.iter().map(|n| n.rect));
        v.extend(self.radios.iter().map(|q| q.hit));
        if let Some(s) = &self.selector {
            v.extend(s.buttons.iter().copied());
        }
        v.extend(self.knobs.iter().map(|k| k.hit));
        v.extend(self.meter.iter().map(|m| m.bar));
        v
    }
}

pub fn hit(l: &Layout, x: i32, y: i32) -> Option<Hit> {
    let plain = [
        (l.grip, Hit::Grip),
        (l.prev, Hit::PrevPreset),
        (l.next, Hit::NextPreset),
        (l.name, Hit::PresetName),
        (l.save, Hit::Save),
        (l.reset, Hit::Reset),
        (l.bypass, Hit::Bypass),
    ];
    if let Some((_, h)) = plain.iter().find(|(r, _)| r.contains(x, y)) {
        return Some(*h);
    }
    if let Some(i) = l.toggles.iter().position(|t| t.hit.contains(x, y)) {
        return Some(Hit::Toggle(i));
    }
    if let Some(i) = l.dropdowns.iter().position(|d| d.rect.contains(x, y)) {
        return Some(Hit::Dropdown(i));
    }
    if let Some(i) = l.numbers.iter().position(|n| n.rect.contains(x, y)) {
        return Some(Hit::Number(i));
    }
    if let Some(i) = l.radios.iter().position(|q| q.hit.contains(x, y)) {
        return Some(Hit::Radio(i));
    }
    if let Some(i) = l.selector.as_ref().and_then(|s| s.buttons.iter().position(|r| r.contains(x, y))) {
        return Some(Hit::Option(i));
    }
    if let Some(k) = l.knobs.iter().find(|k| k.hit.contains(x, y)) {
        return Some(Hit::Knob(k.id));
    }
    l.meter.as_ref().filter(|m| m.bar.contains(x, y)).map(|_| Hit::Meter)
}

/// Angle in degrees (maths convention: 0 = right, counter-clockwise) for a normalized value.
pub fn knob_angle(n: f64) -> f64 {
    let n = if n.is_finite() { n.clamp(0.0, 1.0) } else { 0.0 };
    SWEEP_START_DEG - SWEEP_DEG * n
}

/// A point on a knob's circle of `radius` at `deg` (screen coordinates, y down).
pub fn polar(cx: i32, cy: i32, radius: f64, deg: f64) -> (i32, i32) {
    let a = deg.to_radians();
    ((f64::from(cx) + radius * a.cos()).round() as i32, (f64::from(cy) - radius * a.sin()).round() as i32)
}

/// Points of an arc from value `from` to `to` (normalized), for a polyline.
pub fn arc_points(cx: i32, cy: i32, radius: f64, from: f64, to: f64) -> Vec<(i32, i32)> {
    let (a, b) = (knob_angle(from), knob_angle(to));
    let steps = (((a - b).abs() / 4.0).ceil() as usize).max(1);
    (0..=steps).map(|i| polar(cx, cy, radius, a + (b - a) * i as f64 / steps as f64)).collect()
}

/// New normalized value after a vertical drag of `dy` pixels (up = positive) from `start`;
/// `fine` (Shift) moves five times slower.
pub fn drag_value(start: f64, dy: i32, scale: f64, fine: bool) -> f64 {
    let span = DRAG_SPAN * scale.max(0.1) * if fine { 5.0 } else { 1.0 };
    (start + f64::from(dy) / span).clamp(0.0, 1.0)
}

/// Hover text for what is under the mouse.
pub fn tooltip(spec: &UiSpec, h: Hit) -> &'static str {
    match h {
        Hit::PrevPreset => "Previous preset.",
        Hit::NextPreset => "Next preset.",
        Hit::PresetName => spec.presets_tip,
        Hit::Save => "Save: keep the current settings as the User preset (for this session).",
        Hit::Reset => "Reset: back to the default settings.",
        Hit::Bypass => spec.model.tip(spec.bypass_id),
        Hit::Option(i) => spec.selector.and_then(|s| s.options.get(i)).map_or("", |o| o.tip),
        Hit::Dropdown(i) => spec.dropdowns.get(i).map_or("", |d| spec.model.tip(d.id)),
        Hit::Radio(i) => spec.radios.get(i).map_or("", |q| q.tip),
        Hit::Number(i) => spec.numbers.get(i).map_or("", |n| spec.model.tip(n.id)),
        Hit::Toggle(i) => spec.toggles.get(i).map_or("", |t| spec.model.tip(t.id)),
        Hit::Knob(id) => spec.model.tip(id),
        Hit::Meter => spec.meter.map_or("", |m| m.tip),
        Hit::Grip => "Drag to resize the editor.",
    }
}

fn disjoint(a: &Rect, b: &Rect) -> bool {
    a.right <= b.left || b.right <= a.left || a.bottom <= b.top || b.bottom <= a.top
}

/// Asserts nothing interactive overlaps or leaves the canvas at several sizes (used by every
/// plugin's own layout test).
pub fn assert_layout_ok(spec: &UiSpec) {
    let (dw, dh) = default_size(spec);
    let (mw, mh) = min_size(spec);
    for (w, h) in [(dw, dh), (mw, mh), (1200, spec.base_h + 170), (700, 600)] {
        let l = layout(spec, w, h);
        let rects = l.interactive();
        for (i, a) in rects.iter().enumerate() {
            assert!(a.left >= 0 && a.top >= 0 && a.right <= w && a.bottom <= h, "{w}x{h} {a:?}");
            for b in rects.iter().skip(i + 1) {
                assert!(disjoint(a, b), "{w}x{h} {a:?} {b:?}");
            }
        }
        assert!(l.line_left.1 < l.wordmark.left && l.wordmark.right < l.line_right.0);
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::*;

    struct M;
    impl ParamModel for M {
        fn count(&self) -> usize {
            4
        }
        fn dsp_count(&self) -> usize {
            3
        }
        fn to_plain(&self, _: u32, n: f64) -> f64 {
            n * 10.0 - 5.0
        }
        fn to_normalized(&self, _: u32, v: f64) -> f64 {
            (v + 5.0) / 10.0
        }
        fn default_normalized(&self, _: u32) -> f64 {
            0.5
        }
        fn format(&self, _: u32, n: f64) -> String {
            format!("{n}")
        }
        fn tip(&self, _: u32) -> &'static str {
            "tip"
        }
    }
    static MODEL: M = M;
    static SPEC: UiSpec = UiSpec {
        title: "Test",
        class_name: "T",
        base_w: 640,
        base_h: 330,
        knobs: &[
            KnobSpec { id: 0, caption: "A", ends: ("L", "R"), cx: 100, cy: 130, r: 30, origin: Some(0.0), end_span: None, below: false },
            KnobSpec { id: 1, caption: "B", ends: ("L", "R"), cx: 500, cy: 270, r: 17, origin: None, end_span: None, below: false },
        ],
        toggles: &[ToggleSpec { id: 2, label: "T", cx: 320, cy: 269, glyph: Glyph::Lamp, on_text: "ON", off_text: "OFF" }],
        selector: Some(SelectorSpec {
            id: 2,
            caption: "SEL",
            cx: 320,
            cy: 150,
            spacing: 50,
            options: &[
                SelectorOption { mark: "I", name: "One", tip: "one" },
                SelectorOption { mark: "II", name: "Two", tip: "two" },
                SelectorOption { mark: "III", name: "Three", tip: "three" },
            ],
        }),
        dropdowns: &[],
        numbers: &[],
        radios: &[],
        labels: &[],
        rules: &[],
        theme: crate::spec::Theme::Walnut,
        meter: None,
        bypass_id: 3,
        presets: &[],
        presets_tip: "presets",
        model: &MODEL,
    };

    #[test]
    fn nothing_overlaps_and_everything_fits() {
        assert_layout_ok(&SPEC);
    }

    #[test]
    fn hit_testing_and_tips() {
        let l = layout(&SPEC, 640, 330);
        let at = |r: &Rect| {
            let (x, y) = r.center();
            hit(&l, x, y)
        };
        assert_eq!(at(&l.bypass), Some(Hit::Bypass));
        assert_eq!(at(&l.toggles[0].rect), Some(Hit::Toggle(0)));
        assert_eq!(at(&l.name), Some(Hit::PresetName));
        let sel = l.selector.as_ref().map(|s| s.buttons.clone()).unwrap_or_default();
        assert_eq!(at(&sel[2]), Some(Hit::Option(2)));
        assert_eq!(sel[0].center().0, 270);
        for k in &l.knobs {
            assert_eq!(hit(&l, k.cx, k.cy), Some(Hit::Knob(k.id)));
        }
        assert_eq!(l.knobs[0].origin, 0.5);
        assert_eq!(hit(&l, l.width - 3, l.height - 3), Some(Hit::Grip));
        for h in [Hit::Bypass, Hit::Toggle(0), Hit::Save, Hit::PresetName, Hit::Option(1), Hit::Knob(0), Hit::Grip] {
            assert!(!tooltip(&SPEC, h).is_empty(), "{h:?}");
        }
        assert_eq!(tooltip(&SPEC, Hit::Option(2)), "three");
        assert_eq!(clamp_size(&SPEC, 1, 1_000_000), (480, 990));
    }

    #[test]
    fn knob_geometry_and_drag() {
        assert_eq!(knob_angle(0.0), 225.0);
        assert_eq!(knob_angle(1.0), -45.0);
        assert_eq!(knob_angle(0.5), 90.0, "middle points straight up");
        assert_eq!(polar(100, 100, 10.0, 90.0), (100, 90));
        let pts = arc_points(0, 0, 10.0, 0.0, 1.0);
        assert!(pts.len() > 10 && pts.first() == Some(&polar(0, 0, 10.0, 225.0)));
        assert_eq!(drag_value(0.5, 0, 1.0, false), 0.5);
        assert!((drag_value(0.0, 110, 1.0, false) - 0.5).abs() < 1e-9);
        assert!((drag_value(0.0, 110, 1.0, true) - 0.1).abs() < 1e-9);
        assert_eq!(drag_value(0.9, 500, 1.0, false), 1.0);
        assert_eq!(drag_value(0.1, -500, 2.0, false), 0.0);
    }
}
