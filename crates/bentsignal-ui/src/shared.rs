//! Values shared by the processor (audio thread), the controller and the editor (UI thread).

use crate::spec::ParamModel;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicUsize, Ordering};

/// Normalized values as `f64` bits, a generation bumped when a whole state is loaded (the
/// processor then re-applies every value), a dirty flag for the editor, the input meter level and
/// the host's component handler (an AddRef'd `IComponentHandler` address, 0 when none; owned by
/// `host`).
pub struct Shared {
    model: &'static dyn ParamModel,
    norm: Vec<AtomicU64>,
    pub generation: AtomicU64,
    pub dirty: AtomicBool,
    pub handler: AtomicUsize,
    /// Sample rate (as `f64` bits) of the last `setupProcessing`.
    pub sample_rate: AtomicU64,
    /// Input peak (linear, `f32` bits) for the editor's meter.
    meter: AtomicU32,
    /// `RestartFlags` the processor wants sent to the host (`restartComponent`, e.g. a latency
    /// change); sent and cleared on the UI thread by `host::flush_restart`. 0 = nothing pending.
    pub restart: AtomicI32,
    /// The editor's size (width in the high 32 bits, height in the low ones; 0 = not set yet),
    /// kept in the plugin state so a reopened editor has the size it was left at.
    editor_size: AtomicU64,
    /// Called after an edit from the editor (UI thread), e.g. to mirror it into synced instances.
    edit_hook: OnceLock<fn(&Shared, u32, f64)>,
}

impl Shared {
    pub fn new(model: &'static dyn ParamModel) -> Self {
        Shared {
            model,
            norm: (0..model.count()).map(|i| AtomicU64::new(model.default_normalized(i as u32).to_bits())).collect(),
            generation: AtomicU64::new(0),
            dirty: AtomicBool::new(true),
            handler: AtomicUsize::new(0),
            sample_rate: AtomicU64::new(48_000f64.to_bits()),
            meter: AtomicU32::new(0),
            restart: AtomicI32::new(0),
            editor_size: AtomicU64::new(0),
            edit_hook: OnceLock::new(),
        }
    }

    /// The editor size last saved (`None` until one is set).
    pub fn saved_size(&self) -> Option<(i32, i32)> {
        let v = self.editor_size.load(Ordering::Relaxed);
        let (w, h) = ((v >> 32) as u32, (v & 0xFFFF_FFFF) as u32);
        (w > 0 && h > 0 && w <= 16_384 && h <= 16_384).then_some((w as i32, h as i32))
    }

    pub fn save_size(&self, w: i32, h: i32) {
        if w > 0 && h > 0 {
            self.editor_size.store((u64::from(w as u32) << 32) | u64::from(h as u32), Ordering::Relaxed);
        }
    }

    /// Installs the edit hook (once; later calls are ignored).
    pub fn set_edit_hook(&self, hook: fn(&Shared, u32, f64)) {
        let _ = self.edit_hook.set(hook);
    }

    /// Runs the edit hook for an edit of `id` to normalized `n`.
    pub fn notify_edit(&self, id: u32, n: f64) {
        if let Some(h) = self.edit_hook.get() {
            h(self, id, n);
        }
    }

    pub fn model(&self) -> &'static dyn ParamModel {
        self.model
    }

    pub fn get(&self, id: u32) -> f64 {
        self.norm.get(id as usize).map(|a| f64::from_bits(a.load(Ordering::Relaxed))).unwrap_or(0.0)
    }

    /// Stores a normalized value (clamped) and marks the editor dirty. `false` for unknown ids.
    pub fn set(&self, id: u32, n: f64) -> bool {
        let Some(slot) = self.norm.get(id as usize) else { return false };
        let n = if n.is_finite() { n.clamp(0.0, 1.0) } else { 0.0 };
        slot.store(n.to_bits(), Ordering::Relaxed);
        self.dirty.store(true, Ordering::Relaxed);
        true
    }

    pub fn plain(&self, id: u32) -> f64 {
        self.model.to_plain(id, self.get(id))
    }

    /// Every value, normalized, in id order.
    pub fn snapshot(&self) -> Vec<f64> {
        (0..self.norm.len() as u32).map(|i| self.get(i)).collect()
    }

    /// Replaces every value (a loaded state) and asks the processor to re-apply them.
    pub fn load(&self, values: &[f64]) {
        for (i, v) in values.iter().enumerate() {
            self.set(i as u32, *v);
        }
        self.generation.fetch_add(1, Ordering::Release);
    }

    pub fn meter(&self) -> f32 {
        f32::from_bits(self.meter.load(Ordering::Relaxed))
    }

    pub fn set_meter(&self, peak: f32) {
        self.meter.store(if peak.is_finite() { peak.max(0.0) } else { 0.0 }.to_bits(), Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct M;
    impl ParamModel for M {
        fn count(&self) -> usize {
            2
        }
        fn dsp_count(&self) -> usize {
            1
        }
        fn to_plain(&self, _: u32, n: f64) -> f64 {
            n * 10.0
        }
        fn to_normalized(&self, _: u32, v: f64) -> f64 {
            v / 10.0
        }
        fn default_normalized(&self, id: u32) -> f64 {
            if id == 0 { 0.5 } else { 0.0 }
        }
        fn format(&self, _: u32, n: f64) -> String {
            format!("{n}")
        }
        fn tip(&self, _: u32) -> &'static str {
            "t"
        }
    }
    static MODEL: M = M;

    #[test]
    fn defaults_clamp_and_load() {
        let s = Shared::new(&MODEL);
        assert_eq!(s.snapshot(), [0.5, 0.0]);
        assert!(s.set(1, 7.0) && !s.set(2, 0.5));
        assert_eq!(s.get(1), 1.0);
        s.set(0, f64::NAN);
        assert_eq!(s.get(0), 0.0);
        s.load(&[0.2, 0.4]);
        assert_eq!((s.plain(0), s.generation.load(Ordering::Relaxed)), (2.0, 1));
        s.set_meter(f32::NAN);
        assert_eq!(s.meter(), 0.0);
    }
}
