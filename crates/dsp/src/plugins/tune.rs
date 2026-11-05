//! Plumb: realtime monophonic pitch correction. The input's pitch is tracked with YIN
//! ([`crate::pitch_detect`]) and the output is moved to the nearest note of the chosen key and
//! scale, with every note computed from the A4 reference.
//!
//! Signal path:
//! - Analysis: the channels are summed to mono, decimated to at most ~48 kHz and analysed every
//!   hop (~5.3 ms) over a 26 ms window (80..1200 Hz). Each hop stores the deviation of the
//!   detected pitch from its target note (semitones), the period and a voiced flag. Short
//!   dropouts (up to 8 hops) keep the last voiced result.
//! - Alignment: the audio leaves through a delay of about `CENTER_MS` (+ the lookahead in Lock
//!   mode), so the correction for the audio being read is taken from the hop whose analysis
//!   window is centred on that audio. The analysis therefore sees a little of the future.
//! - Correction: `target = -(1 - flex) * deviation` semitones. Retune Speed > 0 runs `target`
//!   through two cascaded one-pole low-passes (critically damped, time constant `speed / 4.74`), so a step glides smoothly and settles to 95% in
//!   `speed` ms; 0 snaps at once. Retune Speed < 0 is Lock: the audio is delayed by
//!   `LOCK_BASE_MS` + |speed| ms, only hops that agree with both neighbours are used, and at a
//!   note change (a gap between steady hops) the correction switches from the old note's to the
//!   new note's at once, halfway through the gap (within one hop of the real onset), no ramp.
//! - Shifter: one read head on a delay line, moving at `1 - ratio` samples per sample. When it
//!   leaves its range it splices back by whole detected periods, refined (+-P/8) to the position
//!   whose recent past correlates best with the head's (WSOLA-style), with a short raised-cosine
//!   crossfade normalised for that correlation (equal gain when aligned, equal power when not).
//!   Most of the time only one head sounds, so there is no standing comb filter; corrections
//!   under `DEAD_BAND` (3 ct) leave the output as the plain delayed input.
//! - Record mode: causal (the newest analysis hop, no lookahead, no Lock). The head rests
//!   `RECORD_MS` (0.5 ms, the reported latency) behind the input and sweeps up to one period
//!   behind it before splicing, so while correcting the delay averages 0.5 ms + P/2.
//! - Unvoiced or silent input fades (10 ms) to a plain tap at the latency, and the shifter is
//!   re-centred there, so unpitched sounds pass untouched.

use crate::params::{Params, choice, lin, param_plumbing};
use crate::pitch_detect::{YinScratch, yin_with};
use crate::util::{DelayLine, frames_in, ms_to_samples, sane_channels, sane_sr, time_coef};
use crate::{Category, ParamInfo, Plugin, PluginInfo, Unit};

/// Base delay of the shifter (the latency with Retune Speed >= 0).
pub const CENTER_MS: f32 = 20.0;
/// Base delay in Lock mode (Retune Speed < 0): one analysis window plus three hops and the
/// shifter's sway, so a note change is bracketed by steady windows on both sides before the
/// audio around it is read.
pub const LOCK_BASE_MS: f32 = 45.0;
/// Two neighbouring hops agree (one steady note) when their deviations differ less than this.
const STEADY_SEMITONES: f32 = 0.15;
/// Retune Speed range: negative values are Lock lookahead, positive ones smoothing time.
/// Two cascaded one-poles (critically damped: no instant start, no long tail) reach 95% after
/// 4.74 time constants.
const SETTLE_TAUS: f32 = 4.74;
pub const SPEED_MIN_MS: f32 = -3.0;
pub const SPEED_MAX_MS: f32 = 400.0;
const WINDOW_MS: f32 = 26.0;
const HOP_MS: f32 = 5.3;
const MIN_HZ: f32 = 80.0;
const MAX_HZ: f32 = 1200.0;
const YIN_THRESHOLD: f32 = 0.15;
/// RMS below this (about -50 dBFS) is treated as unvoiced.
const GATE_RMS: f32 = 0.003;
/// Hops a dropout may last before the input counts as unvoiced.
const HOLD_HOPS: u64 = 8;
/// Remembered analysis hops (about 340 ms).
const RING: usize = 64;
/// Normal and Lock: the read head sweeps about this far (rounded to whole periods) before it
/// splices back.
const GRAIN_MS: f32 = 12.0;
/// Record mode: the read head rests this far behind the input (the latency).
pub const RECORD_MS: f32 = 0.5;
const RECORD_MIN_SAMPLES: usize = 8;
const HOME_FADE_MS: f32 = 10.0;
/// Record mode analysis window, in periods, and its floor in decimated samples (4 ms at 48 kHz).
const RECORD_WINDOW_PERIODS: f32 = 2.2;
const RECORD_MIN_WINDOW: usize = 192;
/// Delay-line length.
const LINE_MS: f32 = 100.0;
/// Hysteresis (semitones) before the target note changes to a nearer one.
const NOTE_HYSTERESIS: f32 = 0.1;
/// Corrections smaller than this (semitones, 3 cents: below what anyone hears, but the size of
/// the detector's jitter on noisy input) leave the head standing, so the output stays the
/// untouched delayed input instead of a copy that wanders by a few cents.
const DEAD_BAND: f32 = 0.03;
/// Record mode dead band (5 cents): its short analysis window is a little noisier, and a
/// correction that small is not worth a splice.
const RECORD_DEAD_BAND: f32 = 0.05;
/// Samples per sample the head creeps back to the plain tap while the voice is in tune (a
/// pitch error of about 7 cents). Creeping keeps the voice phase-coherent; a fade from a head
/// that sits a fraction of a period off the tap would cancel the upper harmonics (a quick duck).
const RECENTER_SLEW: f32 = 0.004;
/// Largest correction applied, semitones.
const MAX_CORRECTION: f32 = 6.0;

pub const KEYS: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
pub const SCALES: [&str; 13] = [
    "Chromatic",
    "Major",
    "Minor",
    "Harmonic Minor",
    "Melodic Minor",
    "Dorian",
    "Phrygian",
    "Lydian",
    "Mixolydian",
    "Locrian",
    "Major Pentatonic",
    "Minor Pentatonic",
    "Blues",
];

/// Scale degrees, semitones above the key.
pub const SCALE_DEGREES: [&[u8]; 13] = [
    &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
    &[0, 2, 4, 5, 7, 9, 11],
    &[0, 2, 3, 5, 7, 8, 10],
    &[0, 2, 3, 5, 7, 8, 11],
    &[0, 2, 3, 5, 7, 9, 11],
    &[0, 2, 3, 5, 7, 9, 10],
    &[0, 1, 3, 5, 7, 8, 10],
    &[0, 2, 4, 6, 7, 9, 11],
    &[0, 2, 4, 5, 7, 9, 10],
    &[0, 1, 3, 5, 6, 8, 10],
    &[0, 2, 4, 7, 9],
    &[0, 3, 5, 7, 10],
    &[0, 3, 5, 6, 7, 10],
];

/// Modes: Normal (20 ms, or Lock) and Record (lowest latency, causal).
pub const MODES: [&str; 2] = ["Normal", "Record"];

static TUNE_PARAMS: [ParamInfo; 6] = [
    choice("key", "Key", &KEYS, 0),
    choice("scale", "Scale", &SCALES, 1),
    lin("speed", "Retune Speed", SPEED_MIN_MS, SPEED_MAX_MS, 20.0, Unit::Ms),
    lin("flex", "Flex Tune", 0.0, 100.0, 0.0, Unit::Percent),
    lin("reference", "Reference", 420.0, 460.0, 440.0, Unit::Hz),
    choice("mode", "Mode", &MODES, 0),
];

pub static TUNE_INFO: PluginInfo = PluginInfo {
    id: "plumb",
    name: "Plumb",
    short_name: "Plumb",
    category: Category::PitchShift,
    params: &TUNE_PARAMS,
    is_instrument: false,
    audiosuite: true,
};

/// The Lock lookahead in samples for a Retune Speed (0 for speeds >= 0).
pub fn lookahead_samples(sample_rate: f32, speed_ms: f32) -> usize {
    let s = if speed_ms.is_finite() { speed_ms.clamp(SPEED_MIN_MS, SPEED_MAX_MS) } else { 0.0 };
    if s < 0.0 { ms_to_samples(-s, sane_sr(sample_rate)).round() as usize } else { 0 }
}

/// The plugin's latency in samples at a sample rate, Retune Speed and mode: in Record mode the
/// read head's resting distance behind the input; otherwise the shifter's base delay, or in
/// Lock mode the Lock base delay plus the lookahead.
pub fn latency_samples(sample_rate: f32, speed_ms: f32, record: bool) -> usize {
    let sr = sane_sr(sample_rate);
    if record {
        return (ms_to_samples(RECORD_MS, sr).round() as usize).max(RECORD_MIN_SAMPLES);
    }
    let la = lookahead_samples(sr, speed_ms);
    let lock = speed_ms.is_finite() && speed_ms < 0.0;
    let base = if lock { LOCK_BASE_MS } else { CENTER_MS };
    ms_to_samples(base, sr).round() as usize + la
}

/// Bit mask of the pitch classes (C = bit 0) in a key and scale.
pub fn scale_mask(key: usize, scale: usize) -> u16 {
    let degrees = SCALE_DEGREES.get(scale).copied().unwrap_or(&[]);
    degrees.iter().fold(0u16, |m, d| m | 1 << ((usize::from(*d) + key) % 12))
}

/// Fractional MIDI note of a frequency for an A4 reference.
pub fn hz_to_note(hz: f32, reference: f32) -> f32 {
    69.0 + 12.0 * (hz / reference).log2()
}

/// Frequency of a (fractional) MIDI note for an A4 reference.
pub fn note_to_hz(note: f32, reference: f32) -> f32 {
    reference * 2f32.powf((note - 69.0) / 12.0)
}

/// The in-scale note nearest to `note` (`None` for an empty mask).
pub fn nearest_note(note: f32, mask: u16) -> Option<i32> {
    if !note.is_finite() || mask & 0x0fff == 0 {
        return None;
    }
    let base = note.floor() as i32;
    (base - 7..=base + 7).filter(|n| mask >> n.rem_euclid(12) & 1 == 1).min_by(|a, b| (*a as f32 - note).abs().total_cmp(&(*b as f32 - note).abs()))
}

#[derive(Debug, Clone, Copy, Default)]
struct Entry {
    k: u64,
    voiced: bool,
    /// Target note (MIDI).
    note: i32,
    /// Detected pitch minus its target note, semitones.
    dev: f32,
    /// Period in input samples.
    period: f32,
}

pub struct Tune {
    p: Params,
    sr: f32,
    lines: Vec<DelayLine>,
    // Analysis.
    dec: usize,
    win: usize,
    hop: usize,
    ring: Vec<f32>,
    ring_pos: usize,
    acc: f32,
    acc_n: usize,
    ana_count: u64,
    frame: Vec<f32>,
    scratch: YinScratch,
    entries: [Entry; RING],
    latest: Option<u64>,
    last_voiced: Option<(u64, i32, f32, f32)>,
    prev_note: Option<i32>,
    // Parameters as used.
    mask: u16,
    flex: f32,
    reference: f32,
    coef: f32,
    lock: bool,
    record: bool,
    homing: bool,
    // Shifter: one read head; at a splice the old head fades out against the new one.
    t: u64,
    home_d: f32,
    head: f32,
    old: f32,
    xf: f32,
    xf_step: f32,
    xf_corr: f32,
    period: f32,
    corr: f32,
    /// First stage of the two-stage (critically damped) retune glide; `corr` is the second.
    corr_pre: f32,
    home: f32,
    home_step: f32,
    line_max: f32,
    #[cfg(test)]
    probe: (f32, f32),
    #[cfg(test)]
    stats: (usize, f64, usize),
}

impl Default for Tune {
    fn default() -> Self {
        Self::new()
    }
}

impl Tune {
    pub fn new() -> Self {
        let mut s = Self {
            p: Params::new(&TUNE_PARAMS),
            sr: 48_000.0,
            lines: Vec::new(),
            dec: 1,
            win: 1248,
            hop: 256,
            ring: Vec::new(),
            ring_pos: 0,
            acc: 0.0,
            acc_n: 0,
            ana_count: 0,
            frame: Vec::new(),
            scratch: YinScratch::default(),
            entries: [Entry::default(); RING],
            latest: None,
            last_voiced: None,
            prev_note: None,
            mask: 0x0fff,
            flex: 0.0,
            reference: 440.0,
            coef: 0.0,
            lock: false,
            record: false,
            homing: true,
            t: 0,
            home_d: 960.0,
            head: 960.0,
            old: 960.0,
            xf: 1.0,
            xf_step: 1.0,
            xf_corr: 1.0,
            period: 240.0,
            corr: 0.0,
            corr_pre: 0.0,
            home: 1.0,
            home_step: 0.01,
            line_max: 4096.0,
            #[cfg(test)]
            probe: (0.0, 0.0),
            #[cfg(test)]
            stats: (0, 0.0, 0),
        };
        s.update();
        s.home_taps();
        s
    }

    fn update(&mut self) {
        self.mask = scale_mask(self.p.choice(0), self.p.choice(1));
        let record = self.p.choice(5) == 1;
        if record != self.record {
            // Entries are keyed by hop number and Record hops are half as long.
            self.entries = [Entry::default(); RING];
            self.latest = None;
        }
        self.record = record;
        // Record is causal: no Lock (a negative speed is instant).
        let speed = if self.record { self.p.v(2).max(0.0) } else { self.p.v(2) };
        self.lock = speed < 0.0;
        self.coef = if self.lock { 0.0 } else { time_coef(speed / SETTLE_TAUS, self.sr) };
        self.flex = (self.p.v(3) / 100.0).clamp(0.0, 1.0);
        self.reference = self.p.v(4);
        // Tap delay 1 is the newest sample (no delay), so a latency of n reads at n + 1.
        let d = latency_samples(self.sr, speed, self.record) as f32 + 1.0;
        if d != self.home_d {
            // A new latency: re-centre (a jump, like any latency change).
            self.home_d = d;
            self.home_taps();
        }
    }

    /// The read head at the latency, no splice running: the output equals the input delayed by
    /// `home_d`.
    fn home_taps(&mut self) {
        self.head = self.home_d;
        self.old = self.home_d;
        self.xf = 1.0;
    }

    /// Splice length: whole periods (about `GRAIN_MS`; in Record one period).
    fn jump(&self) -> f32 {
        let p = self.period.max(2.0);
        if self.record {
            p.max(4.0)
        } else {
            p * (ms_to_samples(GRAIN_MS, self.sr) / p).round().max(1.0)
        }
    }

    /// The delays the head may sweep before it splices: centred on the latency, or in Record
    /// from the latency up to one jump behind it.
    fn bounds(&self, j: f32) -> (f32, f32) {
        if self.record { (self.home_d, self.home_d + j) } else { ((self.home_d - 0.5 * j).max(2.0), self.home_d + 0.5 * j) }
    }

    /// Normalised correlation of the `m` samples before delays `a` and `b` (channel 0).
    fn similarity(&self, a: f32, b: f32, m: usize) -> f32 {
        let Some(line) = self.lines.first() else { return 1.0 };
        let (mut ab, mut aa, mut bb) = (0.0f32, 0.0f32, 0.0f32);
        for k in 0..m {
            let (x, y) = (line.read(a + k as f32), line.read(b + k as f32));
            ab += x * y;
            aa += x * x;
            bb += y * y;
        }
        if aa > 1e-12 && bb > 1e-12 { ab / (aa * bb).sqrt() } else { 1.0 }
    }

    /// Jumps the head one splice length back toward the far end of its range, to the position
    /// (within a few samples of a whole number of periods) whose recent past matches the head's
    /// best, and starts a crossfade from the old position.
    fn splice(&mut self, rate: f32) {
        let j = self.jump();
        let base = if rate < 0.0 { self.head + j } else { self.head - j };
        self.splice_to(base);
    }

    /// Jumps the head to the position nearest `base` whose recent past matches best (see `splice`).
    fn splice_to(&mut self, base: f32) {
        let p = self.period.max(2.0);
        let reach = (p / 8.0).clamp(1.0, 16.0) as i32;
        let m = (p as usize).clamp(16, 256);
        let (lo, hi) = (2.0, self.line_max - 2.0 - m as f32);
        let mut best = (f32::MIN, base.clamp(lo, hi));
        for k in -reach..=reach {
            let cand = (base + k as f32).clamp(lo, hi);
            let c = self.similarity(self.head, cand, m);
            if c > best.0 {
                best = (c, cand);
            }
        }
        #[cfg(test)]
        {
            self.stats.0 += 1;
        }
        self.old = self.head;
        self.head = best.1;
        self.xf_corr = best.0.clamp(0.0, 1.0);
        let len = p.clamp(32.0, 480.0);
        self.xf_step = 1.0 / len;
        self.xf = 0.0;
    }

    fn pick_note(&mut self, m: f32) -> Option<i32> {
        let near = nearest_note(m, self.mask)?;
        let n = match self.prev_note {
            Some(prev) if self.mask >> prev.rem_euclid(12) & 1 == 1 && (m - prev as f32).abs() <= (m - near as f32).abs() + NOTE_HYSTERESIS => prev,
            _ => near,
        };
        self.prev_note = Some(n);
        Some(n)
    }

    /// Analysis hop in decimated samples: Record analyses three times as often for a fresher estimate.
    fn hop_eff(&self) -> usize {
        if self.record { (self.hop / 3).max(16) } else { self.hop }
    }

    /// Hops a dropout may last before the input counts as unvoiced (the same time at any hop).
    fn hold_hops(&self) -> u64 {
        HOLD_HOPS * (self.hop / self.hop_eff()).max(1) as u64
    }

    /// YIN pitch of the newest `n` decimated samples (`None` below the gate or when unvoiced).
    fn measure(&mut self, n: usize, asr: f32) -> Option<f32> {
        let mask = self.ring.len().wrapping_sub(1);
        let start = self.ring_pos.wrapping_sub(n);
        let mut energy = 0.0f32;
        for (i, f) in self.frame.iter_mut().take(n).enumerate() {
            *f = self.ring.get(start.wrapping_add(i) & mask).copied().unwrap_or(0.0);
            energy += *f * *f;
        }
        let rms = (energy / n.max(1) as f32).sqrt();
        let frame = self.frame.get(..n).unwrap_or(&[]);
        if rms > GATE_RMS { yin_with(frame, asr, MIN_HZ, MAX_HZ, YIN_THRESHOLD, &mut self.scratch) } else { None }
    }

    /// One analysis hop over the newest decimated samples.
    fn analyse(&mut self) {
        let k = self.ana_count / self.hop_eff() as u64;
        let asr = self.sr / self.dec as f32;
        // Record: a short window of about 2.6 periods (the newest samples) keeps the estimate
        // fresh; the full window is the fallback when the short one cannot see the pitch.
        let recent = self.last_voiced.is_some_and(|(kv, ..)| k.saturating_sub(kv) <= self.hold_hops());
        if self.record && !recent && k % 2 == 1 {
            // Searching for a voice: the full window, at the normal hop rate.
            return;
        }
        let short = if self.record && recent { ((RECORD_WINDOW_PERIODS * self.period / self.dec as f32) as usize).clamp(RECORD_MIN_WINDOW.min(self.win), self.win) } else { self.win };
        let mut f0 = self.measure(short, asr);
        if f0.is_none() && short < self.win && recent {
            f0 = self.measure(self.win, asr);
        }
        let mut entry = Entry { k, voiced: false, note: 0, dev: 0.0, period: self.period };
        match f0.filter(|f| f.is_finite() && *f > 0.0) {
            Some(f) => {
                let m = hz_to_note(f, self.reference);
                if let Some(n) = self.pick_note(m) {
                    let dev = (m - n as f32).clamp(-MAX_CORRECTION, MAX_CORRECTION);
                    let period = self.sr / f;
                    entry = Entry { k, voiced: true, note: n, dev, period };
                    self.last_voiced = Some((k, n, dev, period));
                }
            }
            None => match self.last_voiced {
                Some((kv, note, dev, period)) if k.saturating_sub(kv) <= self.hold_hops() => entry = Entry { k, voiced: true, note, dev, period },
                _ => self.prev_note = None,
            },
        }
        if let Some(slot) = self.entries.get_mut((k % RING as u64) as usize) {
            *slot = entry;
        }
        self.latest = Some(k);
    }

    /// The analysis result for audio at input time `tau` (the hop whose window is centred on it,
    /// or the newest one when that hop has not been analysed yet).
    fn lookup(&self, tau: f64) -> Entry {
        let Some(latest) = self.latest else { return Entry::default() };
        let hop_in = (self.hop * self.dec) as f64;
        let half = (self.win * self.dec) as f64 * 0.5;
        let want = ((tau + half) / hop_in).round();
        let k = if want.is_finite() && want > 0.0 { (want as u64).min(latest) } else { 0 };
        let k = k.max(latest.saturating_sub(RING as u64 - 1));
        self.entry(k).unwrap_or_default()
    }

    fn entry(&self, k: u64) -> Option<Entry> {
        self.entries.get((k % RING as u64) as usize).copied().filter(|e| e.k == k && self.latest.is_some_and(|l| k <= l))
    }

    /// Hop `k` agrees with both neighbours: all unvoiced, or all on one note with nearly the same
    /// deviation. A window straddling a note change rarely agrees with both.
    fn steady(&self, k: u64) -> Option<Entry> {
        let (p, a, b) = (self.entry(k.checked_sub(1)?)?, self.entry(k)?, self.entry(k + 1)?);
        let agree = |x: &Entry, y: &Entry| match (x.voiced, y.voiced) {
            (false, false) => true,
            (true, true) => x.note == y.note && (x.dev - y.dev).abs() < STEADY_SEMITONES,
            _ => false,
        };
        (agree(&p, &a) && agree(&a, &b)).then_some(a)
    }

    /// Lock mode: the analysis for audio at input time `tau` from steady hops only. A steady
    /// window covering `tau` wins (the one centred nearest); in a gap between two steady windows
    /// (a note change), the change is placed halfway between the end of the last steady window
    /// before and the start of the first one after, and the correction switches there at once.
    fn lock_lookup(&self, tau: f64) -> Entry {
        let Some(latest) = self.latest else { return Entry::default() };
        let hop_in = (self.hop * self.dec) as f64;
        let win_in = (self.win * self.dec) as f64;
        let near = if tau.is_finite() && tau > 0.0 { (tau / hop_in) as u64 } else { 0 };
        let lo = latest.saturating_sub(RING as u64 - 3).max(near.saturating_sub(16));
        let mut cover: Option<(f64, Entry)> = None;
        let mut before: Option<(f64, Entry)> = None;
        let mut after: Option<(f64, Entry)> = None;
        for k in lo..latest {
            let Some(e) = self.steady(k) else { continue };
            let end = k as f64 * hop_in;
            let start = end - win_in;
            if start <= tau && tau <= end {
                let dist = (tau - (end - 0.5 * win_in)).abs();
                if cover.is_none_or(|(d, _)| dist < d) {
                    cover = Some((dist, e));
                }
            } else if end < tau {
                before = Some((end, e));
            } else if after.is_none() {
                after = Some((start, e));
                break;
            }
        }
        match (cover, before, after) {
            (Some((_, e)), _, _) => e,
            (None, Some((b, eb)), Some((a, ea))) => {
                if tau < 0.5 * (a + b) {
                    eb
                } else {
                    ea
                }
            }
            (None, Some((_, e)), None) | (None, None, Some((_, e))) => e,
            (None, None, None) => self.lookup(tau),
        }
    }
}

impl Plugin for Tune {
    param_plumbing!(TUNE_INFO);

    fn prepare(&mut self, sample_rate: f32, _max_block: usize, channels: usize) {
        self.sr = sane_sr(sample_rate);
        self.dec = ((self.sr / 48_000.0).round() as usize).max(1);
        let asr = self.sr / self.dec as f32;
        self.win = (ms_to_samples(WINDOW_MS, asr).round() as usize).max(128);
        self.hop = (ms_to_samples(HOP_MS, asr).round() as usize).max(16);
        self.ring = vec![0.0; self.win.next_power_of_two()];
        self.frame = vec![0.0; self.win];
        self.scratch.reserve(self.win);
        let line = ms_to_samples(LINE_MS, self.sr) as usize;
        self.lines = (0..sane_channels(channels))
            .map(|_| {
                let mut d = DelayLine::default();
                d.allocate(line);
                d
            })
            .collect();
        self.line_max = self.lines.first().map_or(line as f32, DelayLine::max_delay);
        self.home_step = 1.0 / ms_to_samples(HOME_FADE_MS, self.sr).max(1.0);
        self.period = self.sr / 200.0;
        self.home_d = -1.0;
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.lines.iter_mut().for_each(DelayLine::clear);
        self.ring.iter_mut().for_each(|x| *x = 0.0);
        self.ring_pos = 0;
        self.acc = 0.0;
        self.acc_n = 0;
        self.ana_count = 0;
        self.entries = [Entry::default(); RING];
        self.latest = None;
        self.last_voiced = None;
        self.prev_note = None;
        self.t = 0;
        self.corr = 0.0;
        self.corr_pre = 0.0;
        self.home = 1.0;
        self.homing = true;
        self.home_taps();
    }

    fn process(&mut self, io: &mut [Vec<f32>], frames: usize) {
        let ch = self.lines.len().min(io.len());
        let frames = frames_in(io, frames, ch);
        let ring_mask = self.ring.len().wrapping_sub(1);
        for n in 0..frames {
            // Input: delay lines and the mono analysis feed.
            let mut mono = 0.0f32;
            for c in 0..ch {
                let (Some(buf), Some(line)) = (io.get(c), self.lines.get_mut(c)) else { continue };
                let x = buf.get(n).copied().filter(|x| x.is_finite()).unwrap_or(0.0).clamp(-1e6, 1e6);
                line.push(x);
                mono += x;
            }
            self.t += 1;
            self.acc += mono / ch.max(1) as f32;
            self.acc_n += 1;
            if self.acc_n >= self.dec {
                if let Some(slot) = self.ring.get_mut(self.ring_pos & ring_mask) {
                    *slot = self.acc / self.dec as f32;
                }
                self.ring_pos = self.ring_pos.wrapping_add(1);
                self.acc = 0.0;
                self.acc_n = 0;
                self.ana_count += 1;
                if self.ana_count.is_multiple_of(self.hop_eff() as u64) && self.ana_count >= self.win as u64 {
                    self.analyse();
                }
            }

            // The correction for the audio being read now.
            let shift_d = if self.xf < 1.0 { self.old + (self.head - self.old) * self.xf } else { self.head };
            let d_eff = self.home * self.home_d + (1.0 - self.home) * shift_d;
            let tau = self.t as f64 - f64::from(d_eff);
            let entry = if self.record {
                self.latest.and_then(|k| self.entry(k)).unwrap_or_default()
            } else if self.lock {
                self.lock_lookup(tau)
            } else {
                self.lookup(tau)
            };
            let target = if entry.voiced { -(1.0 - self.flex) * entry.dev } else { 0.0 };
            if entry.voiced {
                self.period = entry.period.clamp(self.sr / MAX_HZ, self.sr / MIN_HZ);
            }
            self.corr_pre = target + (self.corr_pre - target) * self.coef;
            self.corr = self.corr_pre + (self.corr - self.corr_pre) * self.coef;
            if !self.corr.is_finite() {
                self.corr = 0.0;
                self.corr_pre = 0.0;
            }
            // Unvoiced, or a correction inside the dead band: fade to the plain delayed input
            // (with hysteresis, so a correction hovering at the band edge does not flap).
            // A fade from a head that sits off the plain tap would blend two out-of-phase copies of
            // the voice (see `RECENTER_SLEW`), so a small correction only fades once the head is
            // on the tap; until then the head creeps back to it.
            let band = if self.record { RECORD_DEAD_BAND } else { DEAD_BAND };
            let small = target.abs() < if self.homing { 2.0 * band } else { band } && (self.head - self.home_d).abs() < 1.0;
            self.homing = !entry.voiced || small;
            self.home = if self.homing { (self.home + self.home_step).min(1.0) } else { (self.home - self.home_step).max(0.0) };
            if self.home >= 1.0 && (self.head != self.home_d || self.xf < 1.0) {
                self.corr = 0.0;
                self.corr_pre = 0.0;
                self.home_taps();
            }
            #[cfg(test)]
            {
                self.probe = (self.corr, d_eff);
                self.stats.1 += f64::from(self.corr.abs());
                self.stats.2 += 1;
            }

            // In tune but the head is more than half a period off the tap: jump whole periods
            // toward it (a phase-coherent splice) so the creep back is short.
            if !self.homing && entry.voiced && self.corr.abs() < band && self.xf >= 1.0 {
                let p = self.period.max(2.0);
                let d = self.head - self.home_d;
                let n = if self.record { (d / p).floor() } else { (d / p).round() };
                if n != 0.0 && (d.abs() > 0.5 * p || self.record) {
                    self.splice_to(self.head - n * p);
                }
            }

            // Move the head (a correction under a tenth of a cent leaves it standing).
            let ratio = 2f32.powf(self.corr / 12.0);
            let rate = if self.corr.abs() >= band {
                1.0 - ratio
            } else if entry.voiced && !self.homing {
                (self.home_d - self.head).clamp(-RECENTER_SLEW, RECENTER_SLEW)
            } else {
                0.0
            };
            let (dmin, dmax) = (2.0, self.line_max - 2.0);
            self.head = (self.head + rate).clamp(dmin, dmax);
            if self.xf < 1.0 {
                self.old = (self.old + rate).clamp(dmin, dmax);
                self.xf = (self.xf + self.xf_step).min(1.0);
            } else if rate != 0.0 {
                let (lo, hi) = self.bounds(self.jump());
                if (rate < 0.0 && self.head < lo) || (rate > 0.0 && self.head > hi) {
                    self.splice(rate);
                }
            }
            // Raised-cosine crossfade, normalised for the measured correlation of the two
            // positions: equal gain when they match, equal power when they do not.
            let (wo, wn) = if self.xf < 1.0 {
                let w = 0.5 - 0.5 * (std::f32::consts::PI * self.xf).cos();
                let (a, b) = (1.0 - w, w);
                let norm = (a * a + b * b + 2.0 * a * b * self.xf_corr).sqrt().max(1e-3);
                (a / norm, b / norm)
            } else {
                (0.0, 1.0)
            };
            let (head, old) = (self.head, self.old);
            let home = self.home;
            let hd = self.home_d as usize;
            for c in 0..ch {
                let (Some(buf), Some(line)) = (io.get_mut(c), self.lines.get(c)) else { continue };
                let Some(x) = buf.get_mut(n) else { continue };
                let shifted = if wo != 0.0 { wo * line.read_cubic(old) + wn * line.read_cubic(head) } else { line.read_cubic(head) };
                let y = home * line.tap(hd) + (1.0 - home) * shifted;
                *x = if y.is_finite() { y } else { 0.0 };
            }
        }
    }

    fn latency(&self) -> usize {
        (self.home_d as usize).saturating_sub(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pitch_detect::yin;

    const SR: f32 = 48_000.0;

    fn cents(a: f32, b: f32) -> f32 {
        1200.0 * (a / b).log2()
    }

    fn sine(f: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| 0.5 * (i as f32 * f * std::f32::consts::TAU / SR).sin()).collect()
    }

    fn plumb(params: &[(&str, f32)]) -> Tune {
        let mut t = Tune::new();
        t.prepare(SR, 512, 2);
        for (id, v) in params {
            assert!(t.set_param(id, *v), "{id}");
        }
        t.reset();
        t
    }

    fn run(t: &mut Tune, x: &[f32]) -> Vec<f32> {
        let mut out = Vec::with_capacity(x.len());
        for chunk in x.chunks(512) {
            let mut io = vec![chunk.to_vec(), chunk.to_vec()];
            t.process(&mut io, chunk.len());
            assert_eq!(io[0], io[1]);
            out.extend_from_slice(&io[0]);
        }
        out
    }

    /// Median pitch of 2048-sample frames over `range`.
    fn pitch(y: &[f32], range: std::ops::Range<usize>) -> f32 {
        let mut f: Vec<f32> = range.step_by(1024).filter_map(|s| y.get(s..s + 2048)).filter_map(|w| yin(w, SR, 60.0, 1600.0, 0.15)).collect();
        assert!(!f.is_empty());
        f.sort_by(f32::total_cmp);
        f[f.len() / 2]
    }

    #[test]
    fn yin_is_within_one_cent() {
        for f in [110.0, 196.0, 261.63, 440.0, 447.69, 659.25, 987.77] {
            let got = yin(&sine(f, 2048), SR, 60.0, 1600.0, 0.15).unwrap();
            assert!(cents(got, f).abs() < 1.0, "{f} Hz read as {got} ({} ct)", cents(got, f));
        }
    }

    #[test]
    fn scales_and_notes() {
        assert_eq!(scale_mask(0, 1), 0b1010_1011_0101);
        assert_eq!(scale_mask(0, 0), 0x0fff);
        assert_eq!(scale_mask(9, 2), scale_mask(0, 1), "A minor has the notes of C major");
        assert_eq!(nearest_note(69.3, scale_mask(0, 1)), Some(69));
        assert_eq!(nearest_note(70.4, scale_mask(0, 1)), Some(71), "A# is not in C major");
        assert_eq!(nearest_note(65.0, scale_mask(0, 10)), Some(64), "no F in C major pentatonic");
        assert_eq!(nearest_note(66.0, scale_mask(0, 12)), Some(66), "F# is the blue note");
        assert!((note_to_hz(69.0, 432.0) - 432.0).abs() < 1e-3);
        assert!((hz_to_note(440.0, 440.0) - 69.0).abs() < 1e-6);
        assert!(SCALES.len() == SCALE_DEGREES.len() && SCALE_DEGREES.iter().all(|d| d[0] == 0 && d.windows(2).all(|w| w[0] < w[1])));
    }

    #[test]
    fn plus_thirty_cents_snaps_to_a4_at_speed_zero() {
        let mut t = plumb(&[("key", 0.0), ("scale", 1.0), ("speed", 0.0), ("flex", 0.0)]);
        let y = run(&mut t, &sine(440.0 * 2f32.powf(0.3 / 12.0), 48_000));
        let f = pitch(&y, 12_000..44_000);
        assert!(cents(f, 440.0).abs() < 5.0, "{f} Hz, {} ct", cents(f, 440.0));
    }

    #[test]
    fn the_reference_moves_every_target() {
        let mut t = plumb(&[("scale", 1.0), ("speed", 0.0), ("reference", 432.0)]);
        let y = run(&mut t, &sine(432.0 * 2f32.powf(0.3 / 12.0), 48_000));
        let f = pitch(&y, 12_000..44_000);
        assert!(cents(f, 432.0).abs() < 5.0, "{f} Hz, {} ct from 432", cents(f, 432.0));
        // 440 Hz is 31 ct above A4 = 432 Hz, so it is pulled down to 432 too.
        let mut t = plumb(&[("scale", 1.0), ("speed", 0.0), ("reference", 432.0)]);
        let f = pitch(&run(&mut t, &sine(440.0, 48_000)), 12_000..44_000);
        assert!(cents(f, 432.0).abs() < 5.0, "{f} Hz");
    }

    #[test]
    fn flex_100_keeps_the_deviation() {
        let input = 440.0 * 2f32.powf(0.3 / 12.0);
        let mut t = plumb(&[("scale", 1.0), ("speed", 0.0), ("flex", 100.0)]);
        let f = pitch(&run(&mut t, &sine(input, 48_000)), 12_000..44_000);
        assert!(cents(f, input).abs() < 3.0, "{f} Hz, {} ct above A4", cents(f, 440.0));
        let mut t = plumb(&[("scale", 1.0), ("speed", 0.0), ("flex", 50.0)]);
        let f = pitch(&run(&mut t, &sine(input, 48_000)), 12_000..44_000);
        assert!((cents(f, 440.0) - 15.0).abs() < 4.0, "half the deviation passes: {} ct", cents(f, 440.0));
    }

    #[test]
    fn retune_speed_sets_the_settling_time() {
        let mut t = plumb(&[("scale", 1.0), ("speed", 60.0)]);
        let x = sine(440.0 * 2f32.powf(0.3 / 12.0), 24_000);
        let mut track = Vec::new();
        for s in &x {
            let mut io = vec![vec![*s], vec![*s]];
            t.process(&mut io, 1);
            track.push(t.probe.0);
        }
        let first = track.iter().position(|c| c.abs() > 0.003).unwrap();
        let settled = track.iter().position(|c| c.abs() >= 0.95 * 0.3).unwrap();
        let ms = (settled - first) as f32 / SR * 1000.0;
        assert!((ms - 60.0).abs() < 3.0, "95% after {ms} ms");
    }

    #[test]
    fn latency_is_the_delay_of_the_output() {
        for speed in [20.0, 0.0, -1.5, -3.0] {
            let mut t = plumb(&[("speed", speed)]);
            let want = latency_samples(SR, speed, false);
            assert_eq!(t.latency(), want);
            assert_eq!(want, if speed < 0.0 { 2160 } else { 960 } + lookahead_samples(SR, speed));
            let mut x = vec![0.0f32; 8192];
            x[1000] = 1.0;
            let y = run(&mut t, &x);
            let peak = y.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).map(|p| p.0).unwrap();
            assert_eq!(peak, 1000 + want, "speed {speed}");
            assert!((y[peak] - 1.0).abs() < 1e-6);
        }
        assert_eq!(lookahead_samples(SR, -3.0), 144);
        assert_eq!(lookahead_samples(SR, 0.0), 0);
        // Unpitched input at full correction settings still passes delayed and untouched.
        let mut t = plumb(&[("speed", 0.0)]);
        let mut r = 1u32;
        let noise: Vec<f32> = (0..9600)
            .map(|_| {
                r ^= r << 13;
                r ^= r >> 17;
                r ^= r << 5;
                0.0005 * (r as f32 / u32::MAX as f32 - 0.5)
            })
            .collect();
        let y = run(&mut t, &noise);
        for i in 960..9600 {
            assert!((y[i] - noise[i - 960]).abs() < 1e-7, "{i}");
        }
    }

    #[test]
    fn lock_switches_notes_at_the_onset_without_a_glide() {
        // A4 + 30 ct for 0.5 s, then B4 - 25 ct: in C major the targets are A4 and B4.
        let (fa, fb) = (440.0 * 2f32.powf(0.3 / 12.0), 493.88 * 2f32.powf(-0.25 / 12.0));
        let onset = 24_000usize;
        let mut phase = 0.0f32;
        let x: Vec<f32> = (0..48_000)
            .map(|i| {
                phase += if i < onset { fa } else { fb } * std::f32::consts::TAU / SR;
                0.5 * phase.sin()
            })
            .collect();
        let mut t = plumb(&[("scale", 1.0), ("speed", -3.0)]);
        assert_eq!(t.latency(), 2160 + 144);
        let mut y = Vec::new();
        let mut track = Vec::new();
        for s in &x {
            let mut io = vec![vec![*s], vec![*s]];
            t.process(&mut io, 1);
            y.push(io[0][0]);
            track.push(t.probe);
        }
        let hop = t.hop * t.dec;
        let lat = t.latency();
        // Output pitch: A4 up to the onset, B4 from it (one analysis hop of slack each side).
        let a = pitch(&y, 8_000..onset + lat - hop - 2048);
        let b = pitch(&y, onset + lat + hop..46_000);
        assert!(cents(a, 440.0).abs() < 5.0, "before: {a} Hz");
        assert!(cents(b, 493.88).abs() < 5.0, "after: {b} Hz");
        // The correction is either note's, never in between (no glide), and it changes where the
        // audio being read crosses the onset.
        let (ca, cb) = (-0.3f32, 0.25f32);
        let settled = &track[6_000..];
        let between = settled.iter().filter(|(c, _)| (c - ca).abs() > 0.02 && (c - cb).abs() > 0.02).count();
        assert!(between <= hop, "{between} samples off both targets");
        let switch = track.iter().position(|(c, _)| (c - cb).abs() < 0.02).unwrap();
        let audio_time = switch as f32 - track[switch].1;
        assert!((audio_time - onset as f32).abs() <= hop as f32, "switches for audio at {audio_time}, onset {onset}");
    }

    #[test]
    fn lock_is_on_target_from_an_onset_after_silence() {
        let onset = 12_000usize;
        let f = 440.0 * 2f32.powf(0.3 / 12.0);
        let mut x = vec![0.0f32; onset];
        x.extend(sine(f, 36_000));
        let mut t = plumb(&[("scale", 1.0), ("speed", -3.0)]);
        let y = run(&mut t, &x);
        let (lat, hop) = (t.latency(), t.hop * t.dec);
        // The first 2048 samples of output after the onset (plus one hop) are already on A4.
        let w = &y[onset + lat + hop..onset + lat + hop + 2048];
        let got = yin(w, SR, 60.0, 1600.0, 0.15).unwrap();
        assert!(cents(got, 440.0).abs() < 5.0, "{got} Hz at the onset");
        assert!(y[..onset + lat].iter().all(|v| *v == 0.0), "nothing before the delayed onset");
    }

    #[test]
    fn hostile_input_and_parameters_stay_finite() {
        let mut t = plumb(&[]);
        for v in [f32::NAN, f32::INFINITY, -1e9, 1e9] {
            for id in ["key", "scale", "speed", "flex", "reference"] {
                assert!(t.set_param(id, v));
            }
        }
        assert!(!t.set_param("nope", 1.0));
        let mut x = sine(300.0, 20_000);
        x[100] = f32::NAN;
        x[200] = f32::INFINITY;
        x[300] = 1e30;
        for (i, s) in x.iter_mut().enumerate().skip(5000).take(5000) {
            *s = if i % 2 == 0 { 1.0 } else { -1.0 };
        }
        for scale in 0..SCALES.len() {
            t.set_param("scale", scale as f32);
            t.set_param("speed", [-3.0, 0.0, 100.0][scale % 3]);
            t.set_param("flex", 37.0);
            let y = run(&mut t, &x);
            assert!(y.iter().all(|v| v.is_finite() && v.abs() < 1e7), "scale {scale}");
        }
        // Odd rates and channel counts.
        for sr in [8_000.0, 44_100.0, 192_000.0, f32::NAN] {
            let mut t = Tune::new();
            t.prepare(sr, 64, 1);
            let mut io = vec![sine(220.0, 4096)];
            t.process(&mut io, 4096);
            assert!(io[0].iter().all(|v| v.is_finite()));
            assert_eq!(t.latency(), latency_samples(sr, 20.0, false));
        }
        let mut t = Tune::new();
        t.prepare(SR, 64, 0);
        t.process(&mut [], 64);
    }

    /// Welch power spectrum (8192-point Hann frames, hop 2048).
    fn psd(x: &[f32]) -> Vec<f64> {
        use rustfft::{FftPlanner, num_complex::Complex};
        let n = 8192;
        let fft = FftPlanner::<f32>::new().plan_fft_forward(n);
        let w: Vec<f32> = (0..n).map(|i| (std::f32::consts::PI * i as f32 / n as f32).sin().powi(2)).collect();
        let mut acc = vec![0.0f64; n / 2];
        for frame in x.windows(n).step_by(2048) {
            let mut buf: Vec<Complex<f32>> = frame.iter().zip(&w).map(|(a, b)| Complex::new(a * b, 0.0)).collect();
            fft.process(&mut buf);
            for (a, c) in acc.iter_mut().zip(&buf) {
                *a += f64::from(c.norm_sqr());
            }
        }
        acc
    }

    /// Transfer of `out` against `reference` over 1..8 kHz (power ratio per 5.9 Hz bin):
    /// (notch depth = median minus 1st percentile, ripple = 99th minus 1st percentile), dB.
    fn comb(out: &[f32], reference: &[f32]) -> (f32, f32) {
        let (po, pr) = (psd(out), psd(reference));
        let bin = SR / 8192.0;
        let (lo, hi) = ((1000.0 / bin) as usize, (8000.0 / bin) as usize);
        let mut r: Vec<f32> = (lo..hi).map(|k| (10.0 * (po[k] / pr[k].max(1e-30)).log10()) as f32).collect();
        r.sort_by(f32::total_cmp);
        let q = |f: f32| r[((r.len() - 1) as f32 * f) as usize];
        let (min, max, med) = (q(0.01), q(0.99), q(0.5));
        (med - min, max - min)
    }

    /// A voiced test signal: a sine at `f` (0.5) plus white noise (about -26 dB) for 3 s.
    fn voiced(f: f32) -> Vec<f32> {
        let mut r = 7u32;
        sine(f, 144_000)
            .into_iter()
            .map(|s| {
                r ^= r << 13;
                r ^= r >> 17;
                r ^= r << 5;
                s + 0.1 * (r as f32 / u32::MAX as f32 - 0.5)
            })
            .collect()
    }

    /// Envelope ripple (dB, max over min of the 10 ms peaks) of the output for a pure sine.
    fn ripple(y: &[f32]) -> f32 {
        let rms: Vec<f32> = y.chunks(480).map(|c| c.iter().fold(0.0f32, |m, v| m.max(v.abs()))).collect();
        let (mn, mx) = rms.iter().fold((f32::MAX, 0.0f32), |(a, b), v| (a.min(*v), b.max(*v)));
        20.0 * (mx / mn).log10()
    }

    /// Phase report: comb depth of the output (and of output + latency-aligned dry, and
    /// output + undelayed dry) against the dry input, and the crossfade ripple on a sine.
    fn phase_report(params: &[(&str, f32)]) -> Vec<(String, f32, f32, f32)> {
        let mut rows = Vec::new();
        for (name, cents_off) in [("0 ct", 0.0f32), ("+30 ct", 30.0)] {
            let f = 220.0 * 2f32.powf(cents_off / 1200.0);
            let x = voiced(f);
            let mut t = plumb(params);
            let y = run(&mut t, &x);
            let lat = t.latency();
            let (a, b) = (24_000usize, 144_000usize);
            let out = &y[a..b];
            let dry = &x[a - lat..b - lat];
            let (depth, rip) = comb(out, dry);
            let aligned: Vec<f32> = out.iter().zip(dry).map(|(o, d)| o + d).collect();
            let (depth_sum, _) = comb(&aligned, dry);
            let unaligned: Vec<f32> = out.iter().zip(&x[a..b]).map(|(o, d)| o + d).collect();
            let (depth_raw, _) = comb(&unaligned, dry);
            let mean_ct = 100.0 * t.stats.1 / t.stats.2.max(1) as f64;
            // The floor: an ideal shifter (the dry input resampled by the correction ratio).
            let r = 2f64.powf(-f64::from(cents_off) / 1200.0);
            let ideal: Vec<f32> = (a..b)
                .map(|i| {
                    let pos = i as f64 * r;
                    let k = pos as usize;
                    let fr = (pos - k as f64) as f32;
                    x[k] + (x[(k + 1).min(x.len() - 1)] - x[k]) * fr
                })
                .collect();
            let (floor, _) = comb(&ideal, dry);
            eprintln!("  ideal-shift floor {floor:.1} dB");
            eprintln!("  splices {}, mean |correction| {mean_ct:.2} ct, head offset {:.1}", t.stats.0, t.head - t.home_d);
            let mut t = plumb(params);
            let ys = run(&mut t, &sine(f, 144_000));
            let env = ripple(&ys[24_000..]);
            eprintln!(
                "{params:?} {name}: wet notch {depth:.1} dB (ripple {rip:.1}), wet+aligned dry notch {depth_sum:.1} dB, wet+undelayed dry notch {depth_raw:.1} dB, sine envelope ripple {env:.2} dB"
            );
            if cents_off == 0.0 {
                // Near 0 ct the output is the delayed input: no notches, alone or with dry. Record
                // mode analyses a 2-period window (about 3 ct of jitter on this noisy tone) and its
                // head cannot rest on the tap after a splice, so it is held to a looser bound.
                let record = params.iter().any(|(id, v)| *id == "mode" && *v == 1.0);
                let (d_max, s_max) = if record { (3.0, 2.5) } else { (0.8, 0.5) };
                assert!(depth < d_max && depth_sum < s_max, "{params:?} {name}: {depth} {depth_sum}");
            } else {
                // Correcting: no deeper notches than an ideal resampling shifter shows.
                assert!(depth < floor + 0.5, "{params:?} {name}: {depth} vs floor {floor}");
            }
            assert!(env < 0.2, "{params:?} {name}: envelope ripple {env}");
            rows.push((name.to_string(), depth, depth_sum, env));
        }
        rows
    }

    #[test]
    fn phase_metrics() {
        let x = voiced(220.0);
        let same: Vec<f32> = x[24_000..144_000].to_vec();
        let combed: Vec<f32> = (24_000..144_000).map(|i| x[i] + x[i - 960]).collect();
        let half: Vec<f32> = (24_000..144_000).map(|i| x[i] + 0.5 * x[i - 288]).collect();
        eprintln!("calibration: self {:?}, full comb 20 ms {:?}, half comb 6 ms {:?}", comb(&same, &same), comb(&combed, &same), comb(&half, &same));
        for params in
            [vec![("scale", 0.0), ("speed", 0.0)], vec![("scale", 0.0), ("speed", -3.0)], vec![("scale", 0.0), ("speed", 0.0), ("mode", 1.0)]]
        {
            phase_report(&params);
        }
    }

    /// A harmonic-rich tone (20 harmonics, 1/n) at `f`.
    fn buzz(f: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| (1..=20).map(|h| 0.3 * (i as f32 * f * h as f32 * std::f32::consts::TAU / SR).sin() / h as f32).sum()).collect()
    }

    /// Energy off the harmonics of `f0` (outside +-3 bins of each), relative to the total, dB.
    fn off_harmonic_db(y: &[f32], f0: f32) -> f32 {
        let p = psd(y);
        let bin = SR / 8192.0;
        let total: f64 = p.iter().sum();
        let mut on = 0.0f64;
        let mut h = 1.0f32;
        while h * f0 < SR / 2.0 - 4.0 * bin {
            let k = (h * f0 / bin).round() as usize;
            on += p[k.saturating_sub(3)..=(k + 3).min(p.len() - 1)].iter().sum::<f64>();
            h += 1.0;
        }
        (10.0 * ((total - on).max(1e-30) / total).log10()) as f32
    }

    #[test]
    fn record_mode_latency_is_under_one_ms() {
        let mut t = plumb(&[("mode", 1.0), ("speed", -3.0)]);
        assert_eq!(t.latency(), 24, "Record ignores Lock");
        assert_eq!(latency_samples(SR, 20.0, true), 24);
        assert!(t.latency() as f32 / SR * 1000.0 < 1.0);
        let mut x = vec![0.0f32; 4096];
        x[1000] = 1.0;
        let y = run(&mut t, &x);
        let peak = y.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).map(|p| p.0).unwrap();
        assert_eq!(peak, 1024);
        assert!((y[peak] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn record_mode_corrects_a_sine_and_a_low_voice() {
        for (f, target) in [(440.0 * 2f32.powf(0.3 / 12.0), 440.0), (97.999 * 2f32.powf(0.3 / 12.0), 97.999)] {
            let mut t = plumb(&[("mode", 1.0), ("scale", 1.0), ("speed", 0.0)]);
            let x = if f > 200.0 { sine(f, 48_000) } else { buzz(f, 48_000) };
            let y = run(&mut t, &x);
            assert!(y.iter().all(|v| v.is_finite()));
            // The analysis needs one 26 ms window; from 40 ms on the output is on the note.
            let first = yin(&y[1920..1920 + 2048], SR, 60.0, 1600.0, 0.15).unwrap();
            let steady = pitch(&y, 4_800..44_000);
            assert!(cents(first, target).abs() < 5.0, "{f}: {first} Hz at 40 ms");
            assert!(cents(steady, target).abs() < 5.0, "{f}: {steady} Hz");
        }
        let mut t = plumb(&[("mode", 1.0)]);
        let mut x = buzz(150.0, 20_000);
        x[500] = f32::NAN;
        x[600] = f32::INFINITY;
        assert!(run(&mut t, &x).iter().all(|v| v.is_finite()));
    }

    /// Latency against artifacts: mean head delay and off-harmonic energy for Normal and for
    /// Record at several splice lengths, on a sine (440 Hz) and a low voice (98 Hz buzz), both
    /// 30 ct sharp.
    #[test]
    fn record_tradeoff_report() {
        for (name, f, target) in [("sine 440", 440.0 * 2f32.powf(0.3 / 12.0), 440.0f32), ("voice 98", 97.999 * 2f32.powf(0.3 / 12.0), 97.999)] {
            let x = if f > 200.0 { sine(f, 96_000) } else { buzz(f, 96_000) };
            let ideal = if f > 200.0 { sine(target, 96_000) } else { buzz(target, 96_000) };
            eprintln!("{name}: ideal floor {:.1} dB", off_harmonic_db(&ideal[24_000..], target));
            for mode in [0.0, 1.0] {
                let mut t = plumb(&[("mode", mode), ("scale", 0.0), ("speed", 0.0)]);
                let mut y = Vec::new();
                let mut dsum = 0.0f64;
                for (i, s) in x.iter().enumerate() {
                    let mut io = vec![vec![*s], vec![*s]];
                    t.process(&mut io, 1);
                    y.push(io[0][0]);
                    if i >= 24_000 {
                        dsum += f64::from(t.probe.1 - 1.0);
                    }
                }
                let mean_ms = dsum / (x.len() - 24_000) as f64 / f64::from(SR) * 1000.0;
                let err = off_harmonic_db(&y[24_000..], target);
                let got = pitch(&y, 24_000..90_000);
                eprintln!(
                    "  {}: latency {:.2} ms, mean delay {mean_ms:.2} ms, off-harmonic {err:.1} dB, pitch {:+.1} ct, splices {}",
                    if mode > 0.5 { "Record" } else { "Normal" },
                    t.latency() as f32 / SR * 1000.0,
                    cents(got, target),
                    t.stats.0
                );
                if mode > 0.5 {
                    assert!(err < -25.0, "{name}: one-period splices stay clean ({err} dB)");
                }
            }
        }
    }
}
