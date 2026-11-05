//! Grit's parameters as the VST3 host sees them (ids, titles, units, normalized <-> plain
//! mapping), the state blob, factory presets and the editor's hover texts. Safe code only.

use bentsignal_ui::{ParamModel, Preset};
use soundcraft_dsp::plugins::grit::{GRIT_INFO, drive_gain};
use soundcraft_dsp::{ParamInfo, Unit};

/// The values shared by processor, controller and editor.
pub type Shared = bentsignal_ui::Shared;

/// A new `Shared` holding the default values.
pub fn new_shared() -> Shared {
    Shared::new(&MODEL)
}

/// VST3 parameter ids 0..=10 are the DSP parameters in table order; 11 is the host bypass.
pub const DSP_COUNT: usize = 11;
pub const STYLE: u32 = 0;
pub const LOW_CUT: u32 = 1;
pub const THUMP: u32 = 2;
pub const TONE: u32 = 3;
pub const DRIVE: u32 = 4;
pub const PUNISH: u32 = 5;
pub const HIGH_CUT: u32 = 6;
pub const STEEP: u32 = 7;
pub const OUTPUT: u32 = 8;
pub const AUTO: u32 = 9;
pub const MIX: u32 = 10;
pub const BYPASS: u32 = 11;
pub const PARAM_COUNT: usize = 12;

/// State blob: magic, version (LE u32), value count (LE u32), then the plain values (LE f64) in
/// id order.
pub const STATE_MAGIC: [u8; 4] = *b"GRIT";
pub const STATE_VERSION: u32 = 1;
/// Bytes of the fixed header.
pub const STATE_HEADER: usize = 12;

/// The DSP parameter behind a VST3 id (`None` for bypass and unknown ids).
pub fn dsp_param(id: u32) -> Option<&'static ParamInfo> {
    if (id as usize) < DSP_COUNT { GRIT_INFO.params.get(id as usize) } else { None }
}

pub fn is_known(id: u32) -> bool {
    (id as usize) < PARAM_COUNT
}

pub fn title(id: u32) -> &'static str {
    match dsp_param(id) {
        Some(p) => p.name,
        None if id == BYPASS => "Bypass",
        None => "",
    }
}

pub fn units(id: u32) -> &'static str {
    match dsp_param(id).map(|p| p.unit) {
        Some(Unit::Hz) => "Hz",
        Some(Unit::Percent) => "%",
        Some(Unit::Db) => "dB",
        _ => "",
    }
}

/// VST3 step count: 0 continuous, choices - 1 for lists, 1 for switches.
pub fn step_count(id: u32) -> i32 {
    match dsp_param(id) {
        Some(p) if p.unit == Unit::Choice => (p.choices.len() as i32 - 1).max(1),
        Some(p) if p.unit == Unit::Toggle => 1,
        Some(_) => 0,
        None if id == BYPASS => 1,
        None => 0,
    }
}

pub fn is_list(id: u32) -> bool {
    dsp_param(id).is_some_and(|p| p.unit == Unit::Choice)
}

fn finite01(n: f64) -> f64 {
    if n.is_finite() { n.clamp(0.0, 1.0) } else { 0.0 }
}

/// Plain value for a normalized one (the DSP table's own mapping; bypass is 0 or 1).
pub fn to_plain(id: u32, n: f64) -> f64 {
    let n = finite01(n);
    match dsp_param(id) {
        Some(p) => f64::from(p.from_normalized(n as f32)),
        None if id == BYPASS => f64::from(n >= 0.5),
        None => n,
    }
}

/// Normalized value for a plain one (clamped into the range).
pub fn to_normalized(id: u32, v: f64) -> f64 {
    match dsp_param(id) {
        Some(p) => f64::from(p.to_normalized(v as f32)),
        None if id == BYPASS => f64::from(v >= 0.5),
        None => finite01(v),
    }
}

pub fn default_normalized(id: u32) -> f64 {
    match dsp_param(id) {
        Some(p) => f64::from(p.to_normalized(p.default)),
        None => 0.0,
    }
}

/// Display text of a normalized value, with its unit ("Off" for a cut that is out of the way).
pub fn format(id: u32, n: f64) -> String {
    let plain = to_plain(id, n);
    match dsp_param(id) {
        Some(_) if id == LOW_CUT && plain < 1.0 => "Off".to_string(),
        Some(_) if id == HIGH_CUT && plain >= 19_999.0 => "Off".to_string(),
        Some(p) => p.format(plain as f32),
        None if id == BYPASS => if plain >= 0.5 { "On" } else { "Off" }.to_string(),
        None => String::new(),
    }
}

/// Parses text a user typed (a choice name, On/Off, or a number with an optional unit; "k"
/// multiplies by 1000) into a normalized value.
pub fn parse(id: u32, text: &str) -> Option<f64> {
    let t = text.trim();
    let lower = t.to_ascii_lowercase();
    if let Some(p) = dsp_param(id)
        && p.unit == Unit::Choice
        && let Some(i) = p.choices.iter().position(|c| c.eq_ignore_ascii_case(t))
    {
        return Some(to_normalized(id, i as f64));
    }
    let toggle = id == BYPASS || dsp_param(id).is_some_and(|p| p.unit == Unit::Toggle);
    if toggle {
        match lower.as_str() {
            "on" | "yes" | "true" => return Some(1.0),
            "off" | "no" | "false" => return Some(0.0),
            _ => {}
        }
    }
    if lower == "off" {
        match id {
            LOW_CUT => return Some(0.0),
            HIGH_CUT => return Some(1.0),
            _ => {}
        }
    }
    let end = t.find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))).unwrap_or(t.len());
    let num: f64 = t.get(..end)?.trim().replace('\u{2212}', "-").parse().ok()?;
    let rest = lower.get(end..).unwrap_or("").trim_start();
    let v = if rest.starts_with('k') { num * 1000.0 } else { num };
    v.is_finite().then(|| to_normalized(id, v))
}

/// The state blob for normalized `values` (stored as plain values, so a later range change keeps
/// what the user heard).
pub fn encode_state(values: &[f64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(STATE_HEADER + 8 * PARAM_COUNT);
    out.extend_from_slice(&STATE_MAGIC);
    out.extend_from_slice(&STATE_VERSION.to_le_bytes());
    out.extend_from_slice(&(PARAM_COUNT as u32).to_le_bytes());
    for (i, v) in values.iter().enumerate() {
        out.extend_from_slice(&to_plain(i as u32, *v).to_le_bytes());
    }
    out
}

/// Header of a state blob: `(version, value count)`, `None` when it is not ours or too new.
pub fn decode_header(h: &[u8]) -> Option<(u32, usize)> {
    if h.get(..4)? != STATE_MAGIC {
        return None;
    }
    let version = u32::from_le_bytes(h.get(4..8)?.try_into().ok()?);
    let count = u32::from_le_bytes(h.get(8..12)?.try_into().ok()?) as usize;
    ((1..=STATE_VERSION).contains(&version) && count <= 64).then_some((version, count))
}

/// Normalized values from the plain values of a blob (missing ones keep their default; values
/// are clamped into range).
pub fn decode_values(_version: u32, plain: &[f64]) -> [f64; PARAM_COUNT] {
    let mut out: [f64; PARAM_COUNT] = std::array::from_fn(|i| default_normalized(i as u32));
    for (id, (slot, v)) in out.iter_mut().zip(plain).enumerate() {
        if v.is_finite() {
            *slot = to_normalized(id as u32, *v);
        } else {
            *slot = default_normalized(id as u32);
        }
    }
    out
}

/// A whole blob back into normalized values (`None` when it is not a valid Grit state).
pub fn decode_state(bytes: &[u8]) -> Option<[f64; PARAM_COUNT]> {
    let (version, count) = decode_header(bytes)?;
    let body = bytes.get(STATE_HEADER..STATE_HEADER + 8 * count)?;
    let plain: Vec<f64> = body.as_chunks::<8>().0.iter().map(|c| f64::from_le_bytes(*c)).collect();
    Some(decode_values(version, &plain))
}

/// Factory presets: plain values for ids 0..=10 (style, low cut, thump, tone, drive, punish,
/// high cut, steep, output, auto, mix); bypass untouched.
pub static PRESETS: [Preset; 6] = [
    Preset { name: "Default", values: &[0.0, 0.0, 0.0, 0.0, 30.0, 0.0, 20000.0, 0.0, 0.0, 0.0, 100.0] },
    Preset { name: "Warm Tape", values: &[0.0, 40.0, 0.0, -20.0, 45.0, 0.0, 12000.0, 0.0, 0.0, 1.0, 100.0] },
    Preset { name: "Triode Glow", values: &[3.0, 30.0, 0.0, 10.0, 40.0, 0.0, 16000.0, 0.0, 0.0, 1.0, 70.0] },
    Preset { name: "Pentode Bite", values: &[4.0, 80.0, 1.0, 25.0, 55.0, 0.0, 9000.0, 0.0, -2.0, 1.0, 100.0] },
    Preset { name: "Germanium Fuzz", values: &[1.0, 120.0, 0.0, 0.0, 75.0, 0.0, 7000.0, 1.0, -4.0, 1.0, 100.0] },
    Preset { name: "Punished Parallel", values: &[2.0, 150.0, 1.0, 15.0, 50.0, 1.0, 6000.0, 1.0, -6.0, 1.0, 40.0] },
];

/// Hover text of each style.
pub const STYLE_TIPS: [&str; 5] = [
    "Tape: smooth, symmetric saturation. Rounds peaks gently and thickens the lows.",
    "Germanium: lopsided, early-breaking clipping, one half squashes harder. Fuzzy, uneven harmonics.",
    "Console: a wide, forgiving knee. Stays clean longest and adds odd harmonics as it goes.",
    "Triode: asymmetric, mostly even harmonics. Warm and rounded, an octave-up sheen.",
    "Pentode: symmetric with a sharper knee, odd harmonics. Edgier and more forward.",
];

/// Hover text of a parameter.
pub fn tip(id: u32) -> &'static str {
    match id {
        0 => "Style: the saturation curve (Tape, Germanium, Console, Triode or Pentode).",
        1 => "Low Cut: a high-pass before the saturation, so low notes do not drive it. Off at the far left.",
        2 => "Thump: puts a bump at the Low Cut frequency, keeping some weight below the cut.",
        3 => "Tone: a gentle tilt before the saturation. Left is darker, right is brighter.",
        4 => "Drive: gain into the saturation. More drive is more distortion, and louder unless Auto is on.",
        5 => "Punish: adds 20 dB of drive before the saturation.",
        6 => "High Cut: a low-pass after the saturation to tame the top. Off at the far right.",
        7 => "Steep: switches High Cut from 6 dB per octave to a steep 30 dB per octave.",
        8 => "Output: gain after the saturation, applied to the processed signal only, -24 to +12 dB.",
        9 => "Auto: pulls the output level down as Drive goes up, so you can hear the tone and not the volume.",
        10 => "Mix: dry/processed balance for parallel saturation. At 0% the input passes through untouched.",
        11 => "Bypass: passes the dry input through unchanged (crossfaded, no click).",
        _ => "",
    }
}

pub const PRESETS_TIP: &str = "Presets: load one of the built-in starting points.";
pub const METER_TIP: &str = "Input level. The marker shows where the signal starts to hit the saturation: peaks past it are being shaped.";

/// Grit's parameters as the shared editor sees them.
pub struct Model;

pub static MODEL: Model = Model;

impl ParamModel for Model {
    fn count(&self) -> usize {
        PARAM_COUNT
    }
    fn dsp_count(&self) -> usize {
        DSP_COUNT
    }
    fn to_plain(&self, id: u32, n: f64) -> f64 {
        to_plain(id, n)
    }
    fn to_normalized(&self, id: u32, v: f64) -> f64 {
        to_normalized(id, v)
    }
    fn default_normalized(&self, id: u32) -> f64 {
        default_normalized(id)
    }
    fn format(&self, id: u32, n: f64) -> String {
        format(id, n)
    }
    fn tip(&self, id: u32) -> &'static str {
        tip(id)
    }
    /// The input level (dBFS) at which the drive reaches the shaper's knee (gain x level = 1).
    fn meter_marker_db(&self, values: &[f64]) -> Option<f32> {
        let get = |id: u32| values.get(id as usize).copied().unwrap_or(0.0);
        let drive = to_plain(DRIVE, get(DRIVE)) as f32 / 100.0;
        let g = drive_gain(drive, get(PUNISH) >= 0.5);
        Some(-20.0 * g.log10())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_follow_the_dsp_table() {
        let names: Vec<&str> = (0..PARAM_COUNT as u32).map(title).collect();
        assert_eq!(names, ["Style", "Low Cut", "Thump", "Tone", "Drive", "Punish", "High Cut", "Steep", "Output", "Auto", "Mix", "Bypass"]);
        let ids: Vec<&str> = (0..DSP_COUNT as u32).filter_map(dsp_param).map(|p| p.id).collect();
        assert_eq!(ids, ["style", "low_cut", "thump", "tone", "drive", "punish", "high_cut", "steep", "output", "auto_gain", "mix"]);
        assert_eq!((0..PARAM_COUNT as u32).map(step_count).collect::<Vec<_>>(), [4, 0, 1, 0, 0, 1, 0, 1, 0, 1, 0, 1]);
        assert!(!is_known(PARAM_COUNT as u32));
        assert!((0..PARAM_COUNT as u32).all(|id| !tip(id).is_empty()));
        assert!(STYLE_TIPS.iter().all(|t| t.len() > 20));
    }

    #[test]
    fn ranges_text_and_defaults() {
        let close = |a: f64, b: f64| (a - b).abs() < 1e-2;
        assert!(close(to_plain(HIGH_CUT, 0.0), 1000.0) && close(to_plain(HIGH_CUT, 1.0), 20000.0));
        assert!(close(to_plain(TONE, 0.5), 0.0) && close(to_plain(OUTPUT, 1.0), 12.0));
        assert!(close(to_plain(HIGH_CUT, default_normalized(HIGH_CUT)), 20000.0));
        assert_eq!(format(LOW_CUT, 0.0), "Off");
        assert_eq!(format(HIGH_CUT, 1.0), "Off");
        assert_eq!(format(STYLE, 0.75), "Triode");
        assert_eq!(format(DRIVE, default_normalized(DRIVE)), "30%");
        assert_eq!(parse(HIGH_CUT, "off"), Some(1.0));
        assert_eq!(parse(STYLE, "pentode"), Some(1.0));
        assert_eq!(parse(AUTO, "On"), Some(1.0));
        assert!(parse(LOW_CUT, "120 hz").is_some_and(|n| close(to_plain(LOW_CUT, n), 120.0)));
        assert_eq!(parse(DRIVE, "abc"), None);
    }

    #[test]
    fn normalized_round_trip() {
        for id in 0..PARAM_COUNT as u32 {
            for k in 0..=20 {
                let n = f64::from(k) / 20.0;
                let back = to_normalized(id, to_plain(id, n));
                let steps = step_count(id);
                let want = if steps > 0 { (n * f64::from(steps)).round() / f64::from(steps) } else { n };
                assert!((back - want).abs() < 1e-5, "id {id} n {n} back {back}");
            }
        }
        assert_eq!(to_normalized(DRIVE, f64::NAN), default_normalized(DRIVE), "NaN is the default");
    }

    #[test]
    fn state_round_trip_and_hostile_blobs() {
        let mut v: [f64; PARAM_COUNT] = std::array::from_fn(|i| default_normalized(i as u32));
        v[STYLE as usize] = 1.0;
        v[DRIVE as usize] = 0.8;
        v[AUTO as usize] = 1.0;
        v[BYPASS as usize] = 1.0;
        let blob = encode_state(&v);
        assert_eq!(blob.len(), STATE_HEADER + 8 * PARAM_COUNT);
        let back = decode_state(&blob).unwrap_or_default();
        for (a, b) in v.iter().zip(back.iter()) {
            assert!((a - b).abs() < 1e-6, "{v:?} {back:?}");
        }
        assert_eq!(decode_state(b"nope"), None);
        assert_eq!(decode_state(&blob[..20]), None, "truncated");
        let mut newer = blob.clone();
        newer[4] = 99;
        assert_eq!(decode_state(&newer), None, "a newer version is refused");
        let mut nan = blob.clone();
        nan[STATE_HEADER + 8 * DRIVE as usize..STATE_HEADER + 8 * (DRIVE as usize + 1)].copy_from_slice(&f64::NAN.to_le_bytes());
        assert_eq!(decode_state(&nan).map(|s| s[DRIVE as usize]), Some(default_normalized(DRIVE)), "NaN keeps the default");
    }

    #[test]
    fn presets_are_in_range() {
        for Preset { name, values } in PRESETS.iter() {
            assert_eq!(values.len(), DSP_COUNT, "{name}");
            for (id, v) in values.iter().enumerate() {
                let n = to_normalized(id as u32, *v);
                assert!((to_plain(id as u32, n) - v).abs() < 1e-2, "{name} id {id}");
            }
        }
    }

    #[test]
    fn meter_marker_follows_drive_and_punish() {
        let mut v: Vec<f64> = (0..PARAM_COUNT as u32).map(default_normalized).collect();
        let a = MODEL.meter_marker_db(&v).unwrap_or(0.0);
        assert!(a < 0.0 && a > -30.0, "{a}");
        v[PUNISH as usize] = 1.0;
        let b = MODEL.meter_marker_db(&v).unwrap_or(0.0);
        assert!((a - b - 20.0).abs() < 1e-3, "punish is +20 dB: {a} {b}");
    }
}
