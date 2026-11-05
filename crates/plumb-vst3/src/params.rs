//! Plumb's parameters as the VST3 host sees them (ids, titles, units, normalized <-> plain
//! mapping), the state blob, factory presets and the editor's hover texts. Safe code only.

use bentsignal_ui::{ParamModel, Preset};
use soundcraft_dsp::plugins::tune::{SCALES, TUNE_INFO};
use soundcraft_dsp::{ParamInfo, Unit};

/// The values shared by processor, controller and editor.
pub type Shared = bentsignal_ui::Shared;

/// The latency (samples) the current Retune Speed, Mode and sample rate give.
pub fn latency_of(shared: &Shared) -> u32 {
    let sr = f64::from_bits(shared.sample_rate.load(std::sync::atomic::Ordering::Relaxed));
    soundcraft_dsp::plugins::tune::latency_samples(sr as f32, shared.plain(SPEED) as f32, shared.plain(MODE) >= 0.5) as u32
}

/// A new `Shared` holding the default values.
pub fn new_shared() -> Shared {
    Shared::new(&MODEL)
}

/// VST3 parameter ids 0..=5 are the DSP parameters in table order; 6 is the host bypass.
pub const DSP_COUNT: usize = 6;
pub const KEY: u32 = 0;
pub const SCALE: u32 = 1;
pub const SPEED: u32 = 2;
pub const FLEX: u32 = 3;
pub const REFERENCE: u32 = 4;
pub const MODE: u32 = 5;
pub const BYPASS: u32 = 6;
/// Sync group: 0 Off, 1..=5 link every Plumb in the session that has the same group.
pub const SYNC: u32 = 7;
pub const PARAM_COUNT: usize = 8;
/// Number of sync groups.
pub const SYNC_GROUPS: u32 = 5;

/// The keys as shown: every black key under both its names.
pub const KEY_NAMES: [&str; 12] = ["C", "C#/Db", "D", "D#/Eb", "E", "F", "F#/Gb", "G", "G#/Ab", "A", "A#/Bb", "B"];

/// State blob: magic, version (LE u32), value count (LE u32), then the plain values (LE f64) in
/// id order; from version 3 the editor's width and height (LE u32 each) follow (0 = not set).
pub const STATE_MAGIC: [u8; 4] = *b"PLMB";
/// Version 2 added Mode (id 5) before Bypass; version 3 added Sync (id 7) and the editor size.
pub const STATE_VERSION: u32 = 3;
/// Bytes of the fixed header.
pub const STATE_HEADER: usize = 12;

/// The DSP parameter behind a VST3 id (`None` for bypass and unknown ids).
pub fn dsp_param(id: u32) -> Option<&'static ParamInfo> {
    if (id as usize) < DSP_COUNT { TUNE_INFO.params.get(id as usize) } else { None }
}

pub fn is_known(id: u32) -> bool {
    (id as usize) < PARAM_COUNT
}

pub fn title(id: u32) -> &'static str {
    match dsp_param(id) {
        Some(p) => p.name,
        None if id == BYPASS => "Bypass",
        None if id == SYNC => "Sync",
        None => "",
    }
}

pub fn units(id: u32) -> &'static str {
    match dsp_param(id).map(|p| p.unit) {
        Some(Unit::Hz) => "Hz",
        Some(Unit::Percent) => "%",
        Some(Unit::Ms) => "ms",
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
        None if id == SYNC => SYNC_GROUPS as i32,
        None => 0,
    }
}

pub fn is_list(id: u32) -> bool {
    id == SYNC || dsp_param(id).is_some_and(|p| p.unit == Unit::Choice)
}

fn finite01(n: f64) -> f64 {
    if n.is_finite() { n.clamp(0.0, 1.0) } else { 0.0 }
}

/// Plain value for a normalized one (the DSP table's own mapping; bypass is 0 or 1). Retune Speed is
/// reversed: normalized 0 is the slowest (400 ms), 1 is Lock (-3 ms), so the wheel runs slow to fast.
pub fn to_plain(id: u32, n: f64) -> f64 {
    let n = finite01(n);
    match dsp_param(id) {
        Some(p) => f64::from(p.from_normalized(if id == SPEED { (1.0 - n) as f32 } else { n as f32 })),
        None if id == BYPASS => f64::from(n >= 0.5),
        None if id == SYNC => (n * f64::from(SYNC_GROUPS)).round(),
        None => n,
    }
}

/// Normalized value for a plain one (clamped into the range).
pub fn to_normalized(id: u32, v: f64) -> f64 {
    match dsp_param(id) {
        Some(p) => { let n = f64::from(p.to_normalized(v as f32)); if id == SPEED { 1.0 - n } else { n } }
        None if id == BYPASS => f64::from(v >= 0.5),
        None if id == SYNC => if v.is_finite() { v.round().clamp(0.0, f64::from(SYNC_GROUPS)) / f64::from(SYNC_GROUPS) } else { 0.0 },
        None => finite01(v),
    }
}

pub fn default_normalized(id: u32) -> f64 {
    match dsp_param(id) {
        Some(p) => { let n = f64::from(p.to_normalized(p.default)); if id == SPEED { 1.0 - n } else { n } }
        None => 0.0,
    }
}

/// Display text of a normalized value, with its unit (Retune Speed below 0 reads "Lock").
pub fn format(id: u32, n: f64) -> String {
    let plain = to_plain(id, n);
    match dsp_param(id) {
        Some(_) if id == SPEED && plain < 0.0 => format!("Lock {:.1} ms", -plain),
        Some(_) if id == SPEED && plain < 0.05 => "Instant".to_string(),
        Some(_) if id == REFERENCE => format!("{plain:.1} Hz"),
        Some(_) if id == KEY => KEY_NAMES.get(plain.round().max(0.0) as usize).copied().unwrap_or("").to_string(),
        Some(p) => p.format(plain as f32),
        None if id == BYPASS => if plain >= 0.5 { "On" } else { "Off" }.to_string(),
        None if id == SYNC => if plain < 0.5 { "Off".to_string() } else { format!("Group {}", plain as u32) },
        None => String::new(),
    }
}

/// The short value text under a knob: whole numbers ("Lock 3" below zero).
pub fn format_short(id: u32, n: f64) -> String {
    let plain = to_plain(id, n);
    match id {
        SPEED if plain < 0.0 => format!("Lock {:.0}", -plain),
        SPEED | FLEX => format!("{plain:.0}"),
        _ => format(id, n),
    }
}

/// Parses text a user typed (a choice name, On/Off, "instant", or a number with an optional
/// unit) into a normalized value.
pub fn parse(id: u32, text: &str) -> Option<f64> {
    let t = text.trim();
    let lower = t.to_ascii_lowercase();
    if let Some(p) = dsp_param(id)
        && p.unit == Unit::Choice
        && let Some(i) = p.choices.iter().position(|c| c.eq_ignore_ascii_case(t))
    {
        return Some(to_normalized(id, i as f64));
    }
    if id == KEY
        && let Some(i) = KEY_NAMES.iter().position(|k| k.split('/').any(|name| name.eq_ignore_ascii_case(t)))
    {
        return Some(to_normalized(id, i as f64));
    }
    if id == BYPASS {
        match lower.as_str() {
            "on" | "yes" | "true" => return Some(1.0),
            "off" | "no" | "false" => return Some(0.0),
            _ => {}
        }
    }
    if id == SYNC {
        let g = lower.trim_start_matches("group").trim();
        match g {
            "off" | "none" | "0" => return Some(0.0),
            "1" | "2" | "3" | "4" | "5" => return g.parse::<f64>().ok().map(|v| to_normalized(id, v)),
            _ => return None,
        }
    }
    if id == SPEED {
        if lower == "instant" {
            return Some(to_normalized(id, 0.0));
        }
        if let Some(rest) = lower.strip_prefix("lock") {
            let ms: f64 = rest.trim().trim_end_matches("ms").trim().parse().ok()?;
            return ms.is_finite().then(|| to_normalized(id, -ms.abs()));
        }
    }
    let end = t.find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))).unwrap_or(t.len());
    let num: f64 = t.get(..end)?.trim().replace('\u{2212}', "-").parse().ok()?;
    num.is_finite().then(|| to_normalized(id, num))
}

/// What a state blob holds: the normalized values and the editor size it was saved with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Decoded {
    pub values: [f64; PARAM_COUNT],
    pub size: Option<(i32, i32)>,
}

/// The state blob for normalized `values` (stored as plain values, so a later range change keeps
/// what the user heard) and the editor size.
pub fn encode_state(values: &[f64], size: Option<(i32, i32)>) -> Vec<u8> {
    let mut out = Vec::with_capacity(STATE_HEADER + 8 * PARAM_COUNT + 8);
    out.extend_from_slice(&STATE_MAGIC);
    out.extend_from_slice(&STATE_VERSION.to_le_bytes());
    out.extend_from_slice(&(PARAM_COUNT as u32).to_le_bytes());
    for (i, v) in values.iter().enumerate() {
        out.extend_from_slice(&to_plain(i as u32, *v).to_le_bytes());
    }
    let (w, h) = size.unwrap_or((0, 0));
    out.extend_from_slice(&(w.max(0) as u32).to_le_bytes());
    out.extend_from_slice(&(h.max(0) as u32).to_le_bytes());
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
pub fn decode_values(version: u32, plain: &[f64]) -> [f64; PARAM_COUNT] {
    let mut out: [f64; PARAM_COUNT] = std::array::from_fn(|i| default_normalized(i as u32));
    // Version 1 had no Mode: its sixth value is Bypass.
    let ids: &[u32] = match version {
        0 | 1 => &[0, 1, 2, 3, 4, BYPASS],
        2 => &[0, 1, 2, 3, 4, 5, 6],
        _ => &[0, 1, 2, 3, 4, 5, 6, SYNC],
    };
    for (&id, v) in ids.iter().zip(plain) {
        if let Some(slot) = out.get_mut(id as usize) {
            *slot = if v.is_finite() { to_normalized(id, *v) } else { default_normalized(id) };
        }
    }
    out
}

/// The editor size from the 8 bytes after the values (`None` when unset or implausible).
pub fn decode_size(tail: &[u8]) -> Option<(i32, i32)> {
    let w = u32::from_le_bytes(tail.get(..4)?.try_into().ok()?);
    let h = u32::from_le_bytes(tail.get(4..8)?.try_into().ok()?);
    (w > 0 && h > 0 && w <= 16_384 && h <= 16_384).then_some((w as i32, h as i32))
}

/// A whole blob back into values and size (`None` when it is not a valid Plumb state).
pub fn decode_state(bytes: &[u8]) -> Option<Decoded> {
    let (version, count) = decode_header(bytes)?;
    let end = STATE_HEADER + 8 * count;
    let body = bytes.get(STATE_HEADER..end)?;
    let plain: Vec<f64> = body.as_chunks::<8>().0.iter().map(|c| f64::from_le_bytes(*c)).collect();
    let size = if version >= 3 { bytes.get(end..).and_then(decode_size) } else { None };
    Some(Decoded { values: decode_values(version, &plain), size })
}

/// Factory presets: plain values for ids 0..=4 (key, scale, speed, flex, reference); bypass
/// untouched.
pub static PRESETS: [Preset; 1] = [Preset { name: "Default", values: &[0.0, 1.0, 20.0, 0.0, 440.0, 0.0] }];

/// Short marks of the scales for the selector buttons.
pub const SCALE_MARKS: [&str; 13] = ["CHR", "MAJ", "MIN", "HMI", "MMI", "DOR", "PHR", "LYD", "MIX", "LOC", "PMA", "PMI", "BLU"];

/// Hover text of each scale.
pub const SCALE_TIPS: [&str; 13] = [
    "Chromatic: all twelve notes. Every note is a target.",
    "Major: 1 2 3 4 5 6 7.",
    "Minor (natural): 1 2 b3 4 5 b6 b7.",
    "Harmonic Minor: 1 2 b3 4 5 b6 7.",
    "Melodic Minor: 1 2 b3 4 5 6 7.",
    "Dorian: 1 2 b3 4 5 6 b7.",
    "Phrygian: 1 b2 b3 4 5 b6 b7.",
    "Lydian: 1 2 3 #4 5 6 7.",
    "Mixolydian: 1 2 3 4 5 6 b7.",
    "Locrian: 1 b2 b3 4 b5 b6 b7.",
    "Major Pentatonic: 1 2 3 5 6.",
    "Minor Pentatonic: 1 b3 4 5 b7.",
    "Blues: 1 b3 4 b5 5 b7.",
];

/// Hover text of a parameter.
pub fn tip(id: u32) -> &'static str {
    match id {
        0 => "Key: the root note of the scale the voice is pulled to.",
        1 => "Scale: which notes count as targets, relative to the key.",
        2 => {
            "Retune Speed: how long the move to the target note takes, 0 to 400 ms (0 is instant). Below 0 is Lock: the audio is delayed a little more so each new note is on target from its first moment."
        }
        3 => "Flex Tune: how much of the singer's own deviation from the note is kept. 0% snaps hard; 100% leaves the pitch as sung.",
        4 => "Reference: the frequency of A4, 420 to 460 Hz. Every target note is tuned from it.",
        5 => {
            "Mode: Normal (20 ms latency, or Lock) or Record, which tunes with under 1 ms latency for monitoring while you record (no Lock; a little rougher on low voices)."
        }
        6 => "Bypass: passes the input through unchanged, delayed by the same latency (crossfaded, no click).",
        7 => "Link: every Plumb in this session set to the same dot shares key, scale, retune speed, flex tune, reference and mode. Click a lit dot to unlink.",
        _ => "",
    }
}

pub const PRESETS_TIP: &str = "Presets: load the default settings.";

/// The scale names are the DSP's own.
pub fn scale_name(i: usize) -> &'static str {
    SCALES.get(i).copied().unwrap_or("")
}

/// Plumb's parameters as the shared editor sees them.
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
    fn format_short(&self, id: u32, n: f64) -> String {
        format_short(id, n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_follow_the_dsp_table() {
        let names: Vec<&str> = (0..PARAM_COUNT as u32).map(title).collect();
        assert_eq!(names, ["Key", "Scale", "Retune Speed", "Flex Tune", "Reference", "Mode", "Bypass", "Sync"]);
        let ids: Vec<&str> = (0..DSP_COUNT as u32).filter_map(dsp_param).map(|p| p.id).collect();
        assert_eq!(ids, ["key", "scale", "speed", "flex", "reference", "mode"]);
        assert_eq!((0..PARAM_COUNT as u32).map(step_count).collect::<Vec<_>>(), [11, 12, 0, 0, 0, 1, 1, 5]);
        assert!(!is_known(PARAM_COUNT as u32));
        assert!((0..PARAM_COUNT as u32).all(|id| !tip(id).is_empty()));
        assert_eq!(scale_name(3), "Harmonic Minor");
        assert!(SCALE_TIPS.iter().all(|t| t.len() > 10));
    }

    #[test]
    fn ranges_text_and_defaults() {
        let close = |a: f64, b: f64| (a - b).abs() < 1e-2;
        assert!(close(to_plain(SPEED, 1.0), -3.0) && close(to_plain(SPEED, 0.0), 400.0));
        assert!(close(to_plain(REFERENCE, 0.5), 440.0) && close(to_plain(REFERENCE, default_normalized(REFERENCE)), 440.0));
        assert!(close(to_plain(FLEX, 1.0), 100.0));
        assert_eq!(format(KEY, 1.0), "B");
        assert_eq!(format(KEY, 1.0 / 11.0), "C#/Db");
        assert_eq!(format(KEY, 10.0 / 11.0), "A#/Bb");
        assert_eq!(format(SCALE, default_normalized(SCALE)), "Major");
        assert_eq!(format(SPEED, 1.0), "Lock 3.0 ms");
        assert_eq!(format(SPEED, to_normalized(SPEED, 0.0)), "Instant");
        assert_eq!(format(SPEED, default_normalized(SPEED)), "20.0 ms");
        assert_eq!(format(REFERENCE, 0.0), "420.0 Hz");
        assert_eq!(parse(SCALE, "blues"), Some(1.0));
        assert_eq!(parse(KEY, "c#"), Some(to_normalized(KEY, 1.0)));
        assert_eq!(parse(KEY, "Db"), Some(to_normalized(KEY, 1.0)));
        assert_eq!(parse(KEY, "bb"), Some(to_normalized(KEY, 10.0)));
        assert_eq!(parse(KEY, "B"), Some(to_normalized(KEY, 11.0)));
        assert!(parse(SPEED, "Lock 2 ms").is_some_and(|n| close(to_plain(SPEED, n), -2.0)));
        assert!(parse(SPEED, "instant").is_some_and(|n| close(to_plain(SPEED, n), 0.0)));
        assert!(parse(REFERENCE, "432 Hz").is_some_and(|n| close(to_plain(REFERENCE, n), 432.0)));
        assert_eq!(parse(FLEX, "abc"), None);
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
        assert_eq!(to_normalized(FLEX, f64::NAN), default_normalized(FLEX), "NaN is the default");
    }

    #[test]
    fn state_round_trip_and_hostile_blobs() {
        let mut v: [f64; PARAM_COUNT] = std::array::from_fn(|i| default_normalized(i as u32));
        v[KEY as usize] = 1.0;
        v[SPEED as usize] = 0.99;
        v[FLEX as usize] = 0.4;
        v[BYPASS as usize] = 1.0;
        v[SYNC as usize] = to_normalized(SYNC, 3.0);
        let blob = encode_state(&v, Some((820, 505)));
        assert_eq!(blob.len(), STATE_HEADER + 8 * PARAM_COUNT + 8);
        let back = decode_state(&blob).map(|d| d.values).unwrap_or([0.0; PARAM_COUNT]);
        for (a, b) in v.iter().zip(back.iter()) {
            assert!((a - b).abs() < 1e-6, "{v:?} {back:?}");
        }
        assert_eq!(decode_state(&blob).and_then(|d| d.size), Some((820, 505)));
        assert_eq!(decode_state(&encode_state(&v, None)).and_then(|d| d.size), None, "unset size");
        let mut huge = blob.clone();
        huge[blob.len() - 8..blob.len() - 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(decode_state(&huge).and_then(|d| d.size), None, "implausible size");
        assert_eq!(decode_state(b"nope"), None);
        assert_eq!(decode_state(&blob[..20]), None, "truncated");
        let mut newer = blob.clone();
        newer[4] = 99;
        assert_eq!(decode_state(&newer), None, "a newer version is refused");
        let mut nan = blob.clone();
        nan[STATE_HEADER + 8 * FLEX as usize..STATE_HEADER + 8 * (FLEX as usize + 1)].copy_from_slice(&f64::NAN.to_le_bytes());
        assert_eq!(decode_state(&nan).map(|s| s.values[FLEX as usize]), Some(default_normalized(FLEX)), "NaN keeps the default");
    }

    #[test]
    fn version_1_blobs_keep_bypass() {
        let v1 = [9.0, 2.0, 50.0, 10.0, 432.0, 1.0];
        let n = decode_values(1, &v1);
        assert!((to_plain(BYPASS, n[BYPASS as usize]) - 1.0).abs() < 1e-9);
        assert_eq!(n[MODE as usize], default_normalized(MODE));
        assert!((to_plain(REFERENCE, n[REFERENCE as usize]) - 432.0).abs() < 1e-3);
        assert_eq!(format(MODE, 1.0), "Record");
    }

    #[test]
    fn sync_values_and_text() {
        assert_eq!(format(SYNC, 0.0), "Off");
        assert_eq!(format(SYNC, 1.0), "Group 5");
        assert_eq!(parse(SYNC, "group 3"), Some(to_normalized(SYNC, 3.0)));
        assert_eq!(parse(SYNC, "Off"), Some(0.0));
        assert_eq!(parse(SYNC, "6"), None);
        assert_eq!(to_plain(SYNC, 0.6), 3.0);
        assert_eq!(format_short(SPEED, to_normalized(SPEED, 0.0)), "0");
        assert_eq!(format_short(SPEED, to_normalized(SPEED, 20.0)), "20");
        assert_eq!(format_short(SPEED, to_normalized(SPEED, -3.0)), "Lock 3");
        assert_eq!(format_short(FLEX, 0.5), "50");
        // Blobs from before the sync parameter load with it Off.
        let v2 = [9.0, 2.0, 50.0, 10.0, 432.0, 1.0, 0.0];
        let n = decode_values(2, &v2);
        assert_eq!(to_plain(SYNC, n[SYNC as usize]), 0.0);
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
}
