//! The VST3 COM classes: the factory, and the single-component Plumb (IComponent +
//! IAudioProcessor + IEditController in one object), plus the module exports.
//!
//! Threading (the VST3 model): `process` runs on the audio thread; `setupProcessing`/`setActive`
//! only while not processing; everything else (controller, state, views) on the UI thread. The
//! processor's DSP lives in an `UnsafeCell` touched only by the processing calls; values reach it
//! through `Shared` atomics (whole states) and the host's parameter queues (edits, automation).

use crate::params::{self, BYPASS, MODE, PARAM_COUNT, SPEED, STATE_HEADER, SYNC, Shared};
use crate::sync::{self, Peer};
pub use bentsignal_ui::host::{copy_c8, copy_c16};
use bentsignal_ui::host::{flush_restart, set_handler};
use soundcraft_dsp::Plugin;
use soundcraft_dsp::plugins::tune::{SPEED_MIN_MS, Tune, latency_samples};
use std::cell::UnsafeCell;
use std::ffi::{CStr, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComRef, ComWrapper, uid};

/// The Plumb audio module's class id (fixed forever: hosts store it in their sessions).
pub const PLUMB_CID: TUID = uid(0x8E4C2A71, 0xD3B94F58, 0x9A06E1C2, 0x47F5B83D);
pub const VENDOR: &str = "SoundCraft";
pub const PLUGIN_NAME: &str = "Plumb";
pub const SUB_CATEGORIES: &str = "Fx|Pitch Shift";
/// Parameter queues handled per block (more are applied at the block start only... never: they
/// are dropped; hosts send one queue per changed parameter, and there are 7).
const MAX_QUEUES: usize = 32;
/// Largest block the processor prepares for, whatever the host says.
const MAX_BLOCK_CAP: usize = 1 << 16;

// ---- processor -----------------------------------------------------------------------------

/// The audio-thread side: the DSP and its scratch buffers.
pub(crate) struct Engine {
    pub(crate) dsp: Tune,
    wet: Vec<Vec<f32>>,
    dry: Vec<Vec<f32>>,
    /// The input delayed by the plugin's latency, so bypass stays in time with the output.
    dry_line: Vec<Vec<f32>>,
    dry_pos: usize,
    max_block: usize,
    sample_rate: f64,
    generation: u64,
    bypass_target: f32,
    bypass_mix: f32,
    bypass_step: f32,
    /// Largest input sample seen in the current `process` call (for the input meter).
    block_peak: f32,
}

impl Engine {
    fn new() -> Self {
        Engine {
            dsp: Tune::new(),
            wet: Vec::new(),
            dry: Vec::new(),
            dry_line: Vec::new(),
            dry_pos: 0,
            max_block: 0,
            sample_rate: 48_000.0,
            generation: u64::MAX,
            bypass_target: 0.0,
            bypass_mix: 0.0,
            bypass_step: 1.0 / 480.0,
            block_peak: 0.0,
        }
    }

    /// Allocates for a sample rate and block size (never called while processing).
    pub(crate) fn prepare(&mut self, sample_rate: f64, max_block: usize, shared: &Shared) {
        let sr = if sample_rate.is_finite() && sample_rate > 0.0 { sample_rate } else { 48_000.0 };
        let max = max_block.clamp(1, MAX_BLOCK_CAP);
        self.sample_rate = sr;
        self.max_block = max;
        self.wet = vec![vec![0.0; max]; 2];
        self.dry = vec![vec![0.0; max]; 2];
        let line = (latency_samples(sr as f32, SPEED_MIN_MS, false) + 2).next_power_of_two();
        self.dry_line = vec![vec![0.0; line]; 2];
        self.dry_pos = 0;
        self.dsp.prepare(sr as f32, max, 2);
        // 10 ms bypass crossfade.
        self.bypass_step = (1.0 / (0.01 * sr)) as f32;
        self.apply_all(shared);
        self.dsp.reset();
        self.bypass_mix = self.bypass_target;
    }

    pub(crate) fn apply(&mut self, id: u32, n: f64) {
        if let Some(p) = params::dsp_param(id) {
            self.dsp.set_param(p.id, params::to_plain(id, n) as f32);
        } else if id == BYPASS {
            self.bypass_target = if n >= 0.5 { 1.0 } else { 0.0 };
        }
    }

    fn apply_all(&mut self, shared: &Shared) {
        self.generation = shared.generation.load(Ordering::Acquire);
        for id in 0..PARAM_COUNT as u32 {
            self.apply(id, shared.get(id));
        }
    }

    /// Picks up a state loaded since the last block.
    fn sync(&mut self, shared: &Shared) {
        if shared.generation.load(Ordering::Acquire) != self.generation {
            self.apply_all(shared);
        }
    }

    /// Renders `len` frames from `start`: input channel pointers `ins` (`n_in` of them) to `outs`.
    /// In-place buffers (an input pointer equal to an output one) are fine: every input sample is
    /// read before the output sample at the same index is written.
    ///
    /// # Safety
    /// Every pointer in `ins`/`outs` is valid for `start + len` samples.
    unsafe fn render(&mut self, ins: &[*mut f32], outs: &[*mut f32], start: usize, len: usize) {
        let mut done = 0;
        while done < len {
            let at = start + done;
            if self.max_block == 0 {
                // Not prepared: pass the input through.
                for (c, &o) in outs.iter().enumerate() {
                    let src = if ins.is_empty() { None } else { ins.get(c.min(ins.len() - 1)).copied() };
                    for i in at..start + len {
                        // SAFETY: the caller guarantees both pointers cover `start + len` samples.
                        unsafe { *o.add(i) = src.map(|s| *s.add(i)).unwrap_or(0.0) };
                    }
                }
                return;
            }
            let n = (len - done).min(self.max_block);
            let lat = self.dsp.latency();
            for c in 0..2 {
                let src = if ins.is_empty() { None } else { ins.get(c.min(ins.len() - 1)).copied() };
                let (Some(w), Some(d), Some(line)) = (self.wet.get_mut(c), self.dry.get_mut(c), self.dry_line.get_mut(c)) else { continue };
                let mask = line.len().wrapping_sub(1);
                for (i, (w, d)) in w.iter_mut().zip(d.iter_mut()).take(n).enumerate() {
                    // SAFETY: see the function's contract.
                    let x = src.map(|s| unsafe { *s.add(at + i) }).unwrap_or(0.0);
                    *w = x;
                    let pos = self.dry_pos.wrapping_add(i);
                    if let Some(slot) = line.get_mut(pos & mask) {
                        *slot = if x.is_finite() { x } else { 0.0 };
                    }
                    *d = line.get(pos.wrapping_sub(lat) & mask).copied().unwrap_or(0.0);
                    self.block_peak = self.block_peak.max(x.abs());
                }
            }
            self.dry_pos = self.dry_pos.wrapping_add(n);
            self.dsp.process(&mut self.wet, n);
            for i in 0..n {
                let b = self.bypass_mix;
                if b != self.bypass_target {
                    self.bypass_mix = if b < self.bypass_target {
                        (b + self.bypass_step).min(self.bypass_target)
                    } else {
                        (b - self.bypass_step).max(self.bypass_target)
                    };
                }
                for (c, &o) in outs.iter().enumerate() {
                    let ch = c.min(1);
                    let w = self.wet.get(ch).and_then(|v| v.get(i)).copied().unwrap_or(0.0);
                    let d = self.dry.get(ch).and_then(|v| v.get(i)).copied().unwrap_or(0.0);
                    let y = if c < 2 { w + b * (d - w) } else { 0.0 };
                    // SAFETY: see the function's contract.
                    unsafe { *o.add(at + i) = y };
                }
            }
            done += n;
        }
    }
}

/// The channel pointers of a bus (empty when the bus or its table is missing).
///
/// # Safety
/// `bus` is null or points at `count` valid `AudioBusBuffers`.
unsafe fn bus_channels(bus: *mut AudioBusBuffers, count: i32, out: &mut [*mut f32; 8]) -> usize {
    if bus.is_null() || count < 1 {
        return 0;
    }
    // SAFETY: the caller guarantees `bus` points at a valid first bus.
    let b = unsafe { &mut *bus };
    b.silenceFlags = 0;
    // SAFETY: the 32-bit table is the active union member (we only accept 32-bit processing).
    let table = unsafe { b.__field0.channelBuffers32 };
    if table.is_null() {
        return 0;
    }
    let n = (b.numChannels.max(0) as usize).min(out.len());
    let mut k = 0;
    for (i, slot) in out.iter_mut().enumerate().take(n) {
        // SAFETY: `table` holds `numChannels` pointers.
        let p = unsafe { *table.add(i) };
        if p.is_null() {
            break;
        }
        *slot = p;
        k += 1;
    }
    k
}

/// One parameter queue being walked through a block.
#[derive(Clone, Copy)]
struct Queue {
    queue: *mut IParamValueQueue,
    id: u32,
    count: i32,
    next: i32,
}

impl Queue {
    /// The next unapplied point: (sample offset, normalized value).
    fn peek(&self) -> Option<(i32, f64)> {
        if self.next >= self.count {
            return None;
        }
        // SAFETY: the queue comes from the host's IParameterChanges, valid during `process`.
        let q = unsafe { ComRef::from_raw(self.queue) }?;
        let (mut at, mut v) = (0i32, 0f64);
        // SAFETY: valid queue; `at`/`v` are writable.
        (unsafe { q.getPoint(self.next, &mut at, &mut v) } == kResultOk).then_some((at, v))
    }
}

// ---- the plugin object ---------------------------------------------------------------------

pub struct PlumbPlugin {
    pub(crate) shared: Arc<Shared>,
    pub(crate) engine: UnsafeCell<Engine>,
    /// This instance in the link registry (`sync`); it also holds the latency last reported to
    /// the host (samples).
    pub(crate) peer: Arc<Peer>,
}

impl PlumbPlugin {
    pub fn new() -> Self {
        let shared = Arc::new(params::new_shared());
        shared.set_edit_hook(sync::on_edit);
        let peer = Arc::new(Peer { shared: shared.clone(), latency: AtomicU32::new(Self::latency_of(&shared)) });
        sync::register(&peer);
        PlumbPlugin { shared, engine: UnsafeCell::new(Engine::new()), peer }
    }

    /// The latency the current Retune Speed and sample rate give (the DSP's own formula).
    fn latency_of(shared: &Shared) -> u32 {
        params::latency_of(shared)
    }

    /// Flags a latency change for the host (`restartComponent(kLatencyChanged)`, sent on the UI
    /// thread). Any thread; never allocates.
    fn note_latency(&self, now: u32) {
        if self.peer.latency.swap(now, Ordering::AcqRel) != now {
            self.shared.restart.fetch_or(RestartFlags_::kLatencyChanged as i32, Ordering::AcqRel);
        }
    }

    /// UI thread: re-checks the latency against the shared values and tells the host.
    fn check_latency(&self) {
        self.note_latency(Self::latency_of(&self.shared));
        flush_restart(&self.shared);
    }

    /// The host changed parameter `id` through the processor's queue: keep the shared values
    /// current (hosts that send only the queue) and mirror the change into the link group.
    /// Audio thread: atomics only.
    fn note_param(&self, id: u32, v: f64) {
        if self.shared.set(id, v) {
            sync::rt_changed(&self.shared, id, self.shared.get(id));
        }
    }

    /// A loaded state: the values and the editor size, then the latency, then the link group
    /// (an instance entering a group takes the group's values).
    fn load_decoded(&self, d: &params::Decoded) {
        self.shared.load(&d.values);
        if let Some((w, h)) = d.size {
            self.shared.save_size(w, h);
        }
        self.check_latency();
        sync::changed(&self.shared, SYNC, self.shared.get(SYNC));
    }

    /// The processor state. Only the processing calls use it, never two at once (VST3 contract).
    #[allow(clippy::mut_from_ref)]
    fn engine(&self) -> &mut Engine {
        // SAFETY: see above: the host serialises setupProcessing/setActive/process.
        unsafe { &mut *self.engine.get() }
    }

    /// The output ends one latency after the input.
    fn tail_samples(&self) -> u32 {
        Self::latency_of(&self.shared)
    }
}

impl Default for PlumbPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PlumbPlugin {
    fn drop(&mut self) {
        sync::unregister(&self.peer);
        // SAFETY: null clears the handler; the UI thread owns it.
        unsafe { set_handler(&self.shared, std::ptr::null_mut()) };
    }
}

impl Class for PlumbPlugin {
    type Interfaces = (IComponent, IAudioProcessor, IEditController);
}

/// Reads exactly `buf.len()` bytes from a stream.
///
/// # Safety
/// `s` is a valid stream.
unsafe fn read_exact(s: &ComRef<'_, IBStream>, buf: &mut [u8]) -> bool {
    let mut got = 0usize;
    while got < buf.len() {
        let mut n = 0i32;
        let want = (buf.len() - got).min(i32::MAX as usize) as i32;
        // SAFETY: valid stream; the slice tail is writable for `want` bytes.
        let r = unsafe { s.read(buf.as_mut_ptr().add(got).cast(), want, &mut n) };
        if r != kResultOk || n <= 0 {
            return false;
        }
        got += n as usize;
    }
    true
}

/// Reads a Plumb state blob from a stream.
///
/// # Safety
/// `state` is null or a valid stream.
unsafe fn read_state(state: *mut IBStream) -> Option<params::Decoded> {
    // SAFETY: per the contract.
    let s = unsafe { ComRef::from_raw(state) }?;
    let mut head = [0u8; STATE_HEADER];
    // SAFETY: valid stream.
    if !unsafe { read_exact(&s, &mut head) } {
        return None;
    }
    let (version, count) = params::decode_header(&head)?;
    let mut body = vec![0u8; 8 * count];
    // SAFETY: valid stream.
    if !unsafe { read_exact(&s, &mut body) } {
        return None;
    }
    let plain: Vec<f64> = body.as_chunks::<8>().0.iter().map(|c| f64::from_le_bytes(*c)).collect();
    // Version 3 adds the editor size after the values (a stream that ends here has none).
    let mut tail = [0u8; 8];
    // SAFETY: valid stream.
    let size = if version >= 3 && unsafe { read_exact(&s, &mut tail) } { params::decode_size(&tail) } else { None };
    Some(params::Decoded { values: params::decode_values(version, &plain), size })
}

impl IPluginBaseTrait for PlumbPlugin {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }
    unsafe fn terminate(&self) -> tresult {
        // SAFETY: null clears the handler; the UI thread owns it.
        unsafe { set_handler(&self.shared, std::ptr::null_mut()) };
        kResultOk
    }
}

fn audio_bus(bus: &mut BusInfo, dir: BusDirection, name: &str) {
    bus.mediaType = MediaTypes_::kAudio as MediaType;
    bus.direction = dir;
    bus.channelCount = 2;
    copy_c16(name, &mut bus.name);
    bus.busType = BusTypes_::kMain as BusType;
    bus.flags = BusInfo_::BusFlags_::kDefaultActive as u32;
}

impl IComponentTrait for PlumbPlugin {
    unsafe fn getControllerClassId(&self, _class_id: *mut TUID) -> tresult {
        // Single component: the edit controller is this object.
        kNotImplemented
    }
    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }
    unsafe fn getBusCount(&self, media: MediaType, _dir: BusDirection) -> i32 {
        if media == MediaTypes_::kAudio as MediaType { 1 } else { 0 }
    }
    unsafe fn getBusInfo(&self, media: MediaType, dir: BusDirection, index: i32, bus: *mut BusInfo) -> tresult {
        if media != MediaTypes_::kAudio as MediaType || index != 0 {
            return kInvalidArgument;
        }
        // SAFETY: the host passes a writable BusInfo or null.
        let Some(b) = (unsafe { bus.as_mut() }) else { return kInvalidArgument };
        let name = if dir == BusDirections_::kInput as BusDirection { "Stereo In" } else { "Stereo Out" };
        audio_bus(b, dir, name);
        kResultOk
    }
    unsafe fn getRoutingInfo(&self, _i: *mut RoutingInfo, _o: *mut RoutingInfo) -> tresult {
        kNotImplemented
    }
    unsafe fn activateBus(&self, _m: MediaType, _d: BusDirection, _i: i32, _s: TBool) -> tresult {
        kResultOk
    }
    unsafe fn setActive(&self, state: TBool) -> tresult {
        if state != 0 {
            let e = self.engine();
            let (sr, max) = (e.sample_rate, if e.max_block == 0 { 1024 } else { e.max_block });
            e.prepare(sr, max, &self.shared);
        }
        kResultOk
    }
    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        // SAFETY: the host passes a valid stream or null.
        match unsafe { read_state(state) } {
            Some(d) => {
                self.load_decoded(&d);
                kResultOk
            }
            None => kResultFalse,
        }
    }
    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        // SAFETY: the host passes a valid stream or null.
        let Some(s) = (unsafe { ComRef::from_raw(state) }) else { return kInvalidArgument };
        let blob = params::encode_state(&self.shared.snapshot(), self.shared.saved_size());
        let mut written = 0i32;
        // SAFETY: valid stream; `blob` is readable for its length.
        let r = unsafe { s.write(blob.as_ptr() as *mut c_void, blob.len() as i32, &mut written) };
        if r == kResultOk && written as usize == blob.len() { kResultOk } else { kResultFalse }
    }
}

impl IAudioProcessorTrait for PlumbPlugin {
    unsafe fn setBusArrangements(&self, inputs: *mut SpeakerArrangement, n_in: i32, outputs: *mut SpeakerArrangement, n_out: i32) -> tresult {
        if n_in != 1 || n_out != 1 || inputs.is_null() || outputs.is_null() {
            return kResultFalse;
        }
        // SAFETY: one arrangement each, checked above.
        let (i, o) = unsafe { (*inputs, *outputs) };
        if i == SpeakerArr::kStereo && o == SpeakerArr::kStereo { kResultTrue } else { kResultFalse }
    }
    unsafe fn getBusArrangement(&self, _dir: BusDirection, index: i32, arr: *mut SpeakerArrangement) -> tresult {
        if index != 0 || arr.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: checked non-null; the host passes a writable arrangement.
        unsafe { *arr = SpeakerArr::kStereo };
        kResultOk
    }
    unsafe fn canProcessSampleSize(&self, size: i32) -> tresult {
        if size == SymbolicSampleSizes_::kSample32 as i32 { kResultTrue } else { kResultFalse }
    }
    unsafe fn getLatencySamples(&self) -> u32 {
        // The shifter's base delay, plus the lookahead in Lock mode (Retune Speed below 0): the
        // value last set by the controller or seen by the processor.
        self.peer.latency.load(Ordering::Acquire)
    }
    unsafe fn setupProcessing(&self, setup: *mut ProcessSetup) -> tresult {
        // SAFETY: the host passes a valid setup or null.
        let Some(s) = (unsafe { setup.as_ref() }) else { return kInvalidArgument };
        if s.symbolicSampleSize != SymbolicSampleSizes_::kSample32 as i32 {
            return kResultFalse;
        }
        self.shared.sample_rate.store(s.sampleRate.to_bits(), Ordering::Relaxed);
        // A new rate is a new latency in samples; the host asks for it after setup anyway.
        self.peer.latency.store(Self::latency_of(&self.shared), Ordering::Release);
        self.engine().prepare(s.sampleRate, s.maxSamplesPerBlock.max(1) as usize, &self.shared);
        kResultOk
    }
    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        kResultOk
    }
    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        // SAFETY: the host passes valid process data or null.
        let Some(d) = (unsafe { data.as_mut() }) else { return kInvalidArgument };
        if d.symbolicSampleSize != SymbolicSampleSizes_::kSample32 as i32 {
            return kResultFalse;
        }
        let e = self.engine();
        e.sync(&self.shared);
        let frames = d.numSamples.max(0) as usize;
        e.block_peak = 0.0;

        let mut queues = [Queue { queue: std::ptr::null_mut(), id: 0, count: 0, next: 0 }; MAX_QUEUES];
        let mut nq = 0;
        // SAFETY: the host's parameter changes, valid during `process`, or null.
        if let Some(ch) = unsafe { ComRef::from_raw(d.inputParameterChanges) } {
            // SAFETY: valid changes.
            for i in 0..unsafe { ch.getParameterCount() } {
                // SAFETY: valid changes; index in range.
                let Some(q) = (unsafe { ComRef::from_raw(ch.getParameterData(i)) }) else { continue };
                let Some(slot) = queues.get_mut(nq) else { break };
                // SAFETY: valid queue.
                *slot = Queue { queue: q.as_ptr(), id: unsafe { q.getParameterId() }, count: unsafe { q.getPointCount() }, next: 0 };
                nq += 1;
            }
        }
        let queues = &mut queues[..nq];

        let mut ins = [std::ptr::null_mut(); 8];
        let mut outs = [std::ptr::null_mut(); 8];
        // SAFETY: the host's bus tables, `numInputs`/`numOutputs` long.
        let n_in = unsafe { bus_channels(d.inputs, d.numInputs, &mut ins) };
        // SAFETY: as above.
        let n_out = unsafe { bus_channels(d.outputs, d.numOutputs, &mut outs) };
        let (ins, outs) = (&ins[..n_in], &outs[..n_out]);

        // Sample-accurate: render up to the next point, apply every point at that offset, repeat.
        let mut pos = 0usize;
        loop {
            for q in queues.iter_mut() {
                while let Some((at, v)) = q.peek() {
                    if at.max(0) as usize > pos {
                        break;
                    }
                    e.apply(q.id, v);
                    self.note_param(q.id, v);
                    q.next += 1;
                }
            }
            if pos >= frames {
                break;
            }
            let end = queues.iter().filter_map(|q| q.peek()).map(|(at, _)| (at.max(0) as usize).max(pos + 1)).fold(frames, usize::min);
            if !outs.is_empty() {
                // SAFETY: the host's channel buffers hold `numSamples` frames.
                unsafe { e.render(ins, outs, pos, end - pos) };
            }
            pos = end;
        }
        // Points past the block end (a host bug): apply them anyway.
        for q in queues.iter_mut() {
            while let Some((_, v)) = q.peek() {
                e.apply(q.id, v);
                self.note_param(q.id, v);
                q.next += 1;
            }
        }
        // A Retune Speed change into or out of Lock, or a Mode change, changes the latency:
        // flag it for the host.
        self.note_latency(e.dsp.latency() as u32);
        // Input meter: the block's peak, falling 20 dB per second.
        let sr = e.sample_rate.max(1.0) as f32;
        let fall = 0.1f32.powf(frames as f32 / sr);
        self.shared.set_meter(e.block_peak.max(self.shared.meter() * fall));
        kResultOk
    }
    unsafe fn getTailSamples(&self) -> u32 {
        self.tail_samples()
    }
}

impl IEditControllerTrait for PlumbPlugin {
    unsafe fn setComponentState(&self, state: *mut IBStream) -> tresult {
        // Single component: the values are already shared; re-reading keeps hosts that pass a
        // stored component state here working.
        // SAFETY: the host passes a valid stream or null.
        if let Some(d) = unsafe { read_state(state) } {
            self.load_decoded(&d);
        }
        kResultOk
    }
    unsafe fn setState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }
    unsafe fn getState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }
    unsafe fn getParameterCount(&self) -> i32 {
        PARAM_COUNT as i32
    }
    unsafe fn getParameterInfo(&self, index: i32, info: *mut ParameterInfo) -> tresult {
        if index < 0 || index as usize >= PARAM_COUNT {
            return kInvalidArgument;
        }
        // SAFETY: the host passes a writable ParameterInfo or null.
        let Some(i) = (unsafe { info.as_mut() }) else { return kInvalidArgument };
        let id = index as u32;
        i.id = id;
        copy_c16(params::title(id), &mut i.title);
        copy_c16(params::title(id), &mut i.shortTitle);
        copy_c16(params::units(id), &mut i.units);
        i.stepCount = params::step_count(id);
        i.defaultNormalizedValue = params::default_normalized(id);
        i.unitId = 0;
        let mut flags = ParameterInfo_::ParameterFlags_::kCanAutomate;
        if params::is_list(id) {
            flags |= ParameterInfo_::ParameterFlags_::kIsList;
        }
        if id == BYPASS {
            flags |= ParameterInfo_::ParameterFlags_::kIsBypass;
        }
        i.flags = flags;
        kResultOk
    }
    unsafe fn getParamStringByValue(&self, id: u32, n: f64, string: *mut String128) -> tresult {
        if !params::is_known(id) {
            return kInvalidArgument;
        }
        // SAFETY: the host passes a writable String128 or null.
        let Some(s) = (unsafe { string.as_mut() }) else { return kInvalidArgument };
        copy_c16(&params::format(id, n), s);
        kResultOk
    }
    unsafe fn getParamValueByString(&self, id: u32, string: *mut TChar, value: *mut f64) -> tresult {
        if string.is_null() || value.is_null() || !params::is_known(id) {
            return kInvalidArgument;
        }
        let mut units = Vec::with_capacity(128);
        for i in 0..128 {
            // SAFETY: a null-terminated UTF-16 string from the host (at most String128).
            let c = unsafe { *string.add(i) } as u16;
            if c == 0 {
                break;
            }
            units.push(c);
        }
        let text = String::from_utf16_lossy(&units);
        match params::parse(id, &text) {
            Some(n) => {
                // SAFETY: checked non-null.
                unsafe { *value = n };
                kResultOk
            }
            None => kResultFalse,
        }
    }
    unsafe fn normalizedParamToPlain(&self, id: u32, n: f64) -> f64 {
        params::to_plain(id, n)
    }
    unsafe fn plainParamToNormalized(&self, id: u32, v: f64) -> f64 {
        params::to_normalized(id, v)
    }
    unsafe fn getParamNormalized(&self, id: u32) -> f64 {
        self.shared.get(id)
    }
    unsafe fn setParamNormalized(&self, id: u32, v: f64) -> tresult {
        if !self.shared.set(id, v) {
            return kInvalidArgument;
        }
        if id == SPEED || id == MODE {
            self.check_latency();
        }
        sync::changed(&self.shared, id, self.shared.get(id));
        kResultOk
    }
    unsafe fn setComponentHandler(&self, handler: *mut IComponentHandler) -> tresult {
        // SAFETY: the host hands a valid handler or null.
        unsafe { set_handler(&self.shared, handler) };
        flush_restart(&self.shared);
        kResultOk
    }
    unsafe fn createView(&self, name: FIDString) -> *mut IPlugView {
        if name.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: a null-terminated view name from the host.
        if unsafe { CStr::from_ptr(name) }.to_bytes() != b"editor" {
            return std::ptr::null_mut();
        }
        #[cfg(windows)]
        {
            bentsignal_ui::editor::create_view(self.shared.clone(), &crate::ui::SPEC)
        }
        #[cfg(not(windows))]
        {
            std::ptr::null_mut()
        }
    }
}

// ---- factory -------------------------------------------------------------------------------

pub struct Factory;

impl Class for Factory {
    type Interfaces = (IPluginFactory2,);
}

impl IPluginFactoryTrait for Factory {
    unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
        // SAFETY: the host passes a writable PFactoryInfo or null.
        let Some(i) = (unsafe { info.as_mut() }) else { return kInvalidArgument };
        copy_c8(VENDOR, &mut i.vendor);
        copy_c8("", &mut i.url);
        copy_c8("", &mut i.email);
        i.flags = PFactoryInfo_::FactoryFlags_::kUnicode as i32;
        kResultOk
    }
    unsafe fn countClasses(&self) -> i32 {
        1
    }
    unsafe fn getClassInfo(&self, index: i32, info: *mut PClassInfo) -> tresult {
        if index != 0 {
            return kInvalidArgument;
        }
        // SAFETY: the host passes a writable PClassInfo or null.
        let Some(i) = (unsafe { info.as_mut() }) else { return kInvalidArgument };
        i.cid = PLUMB_CID;
        i.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as i32;
        copy_c8("Audio Module Class", &mut i.category);
        copy_c8(PLUGIN_NAME, &mut i.name);
        kResultOk
    }
    unsafe fn createInstance(&self, cid: FIDString, iid: FIDString, obj: *mut *mut c_void) -> tresult {
        if cid.is_null() || iid.is_null() || obj.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: the host passes a pointer to a 16-byte class id.
        let cid = unsafe { *(cid as *const TUID) };
        if cid != PLUMB_CID {
            // SAFETY: checked non-null.
            unsafe { *obj = std::ptr::null_mut() };
            return kNoInterface;
        }
        let Some(unknown) = ComWrapper::new(PlumbPlugin::new()).to_com_ptr::<FUnknown>() else { return kNoInterface };
        let p = unknown.as_ptr();
        // SAFETY: `p` is our live object; queryInterface AddRefs what it hands out, and `unknown`
        // drops its own reference afterwards.
        unsafe { ((*(*p).vtbl).queryInterface)(p, iid as *const TUID, obj) }
    }
}

impl IPluginFactory2Trait for Factory {
    unsafe fn getClassInfo2(&self, index: i32, info: *mut PClassInfo2) -> tresult {
        if index != 0 {
            return kInvalidArgument;
        }
        // SAFETY: the host passes a writable PClassInfo2 or null.
        let Some(i) = (unsafe { info.as_mut() }) else { return kInvalidArgument };
        i.cid = PLUMB_CID;
        i.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as i32;
        copy_c8("Audio Module Class", &mut i.category);
        copy_c8(PLUGIN_NAME, &mut i.name);
        i.classFlags = 0;
        copy_c8(SUB_CATEGORIES, &mut i.subCategories);
        copy_c8(VENDOR, &mut i.vendor);
        copy_c8(env!("CARGO_PKG_VERSION"), &mut i.version);
        copy_c8("VST 3.7.0", &mut i.sdkVersion);
        kResultOk
    }
}

// ---- module exports ------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "system" fn GetPluginFactory() -> *mut IPluginFactory {
    match ComWrapper::new(Factory).to_com_ptr::<IPluginFactory>() {
        Some(p) => p.into_raw(),
        None => std::ptr::null_mut(),
    }
}

#[cfg(windows)]
#[unsafe(no_mangle)]
pub extern "system" fn InitDll() -> bool {
    true
}

#[cfg(windows)]
#[unsafe(no_mangle)]
pub extern "system" fn ExitDll() -> bool {
    bentsignal_ui::editor::unregister_class();
    true
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub extern "C" fn bundleEntry(_bundle: *mut c_void) -> bool {
    true
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
pub extern "C" fn bundleExit() -> bool {
    true
}

#[cfg(all(unix, not(target_os = "macos")))]
#[unsafe(no_mangle)]
pub extern "C" fn ModuleEntry(_handle: *mut c_void) -> bool {
    true
}

#[cfg(all(unix, not(target_os = "macos")))]
#[unsafe(no_mangle)]
pub extern "C" fn ModuleExit() -> bool {
    true
}
