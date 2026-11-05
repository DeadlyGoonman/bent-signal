//! COM-level tests: the factory, the parameters, the state stream and processing, called the way
//! a host calls them.

use crate::params::{self, BYPASS, FLEX, KEY, MODE, PARAM_COUNT, REFERENCE, SCALE, SPEED};
use crate::plugin::{Factory, PLUMB_CID, PlumbPlugin};
use std::cell::RefCell;
use std::ffi::{CStr, c_void};
use std::sync::Mutex;
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComWrapper};

fn c8(s: &[std::ffi::c_char]) -> String {
    // SAFETY: test buffers written by the plugin are null-terminated.
    unsafe { CStr::from_ptr(s.as_ptr()) }.to_string_lossy().into_owned()
}

fn c16(s: &[TChar]) -> String {
    let n = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..n].iter().map(|&c| c as u16).collect::<Vec<_>>())
}

// ---- host-side test objects ----------------------------------------------------------------

#[derive(Default)]
struct MemStream {
    data: RefCell<Vec<u8>>,
    pos: RefCell<usize>,
}

impl Class for MemStream {
    type Interfaces = (IBStream,);
}

impl IBStreamTrait for MemStream {
    unsafe fn read(&self, buffer: *mut c_void, n: i32, read: *mut i32) -> tresult {
        let data = self.data.borrow();
        let mut pos = self.pos.borrow_mut();
        let k = (n.max(0) as usize).min(data.len() - *pos);
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr().add(*pos), buffer.cast::<u8>(), k) };
        *pos += k;
        if !read.is_null() {
            unsafe { *read = k as i32 };
        }
        kResultOk
    }
    unsafe fn write(&self, buffer: *mut c_void, n: i32, written: *mut i32) -> tresult {
        let bytes = unsafe { std::slice::from_raw_parts(buffer.cast::<u8>(), n.max(0) as usize) };
        self.data.borrow_mut().extend_from_slice(bytes);
        if !written.is_null() {
            unsafe { *written = n };
        }
        kResultOk
    }
    unsafe fn seek(&self, pos: i64, _mode: i32, result: *mut i64) -> tresult {
        *self.pos.borrow_mut() = pos.max(0) as usize;
        if !result.is_null() {
            unsafe { *result = pos };
        }
        kResultOk
    }
    unsafe fn tell(&self, pos: *mut i64) -> tresult {
        unsafe { *pos = *self.pos.borrow() as i64 };
        kResultOk
    }
}

struct Queue {
    id: u32,
    points: Vec<(i32, f64)>,
}

impl Class for Queue {
    type Interfaces = (IParamValueQueue,);
}

impl IParamValueQueueTrait for Queue {
    unsafe fn getParameterId(&self) -> ParamID {
        self.id
    }
    unsafe fn getPointCount(&self) -> i32 {
        self.points.len() as i32
    }
    unsafe fn getPoint(&self, index: i32, offset: *mut i32, value: *mut f64) -> tresult {
        match self.points.get(index as usize) {
            Some(&(o, v)) => {
                unsafe {
                    *offset = o;
                    *value = v;
                }
                kResultOk
            }
            None => kInvalidArgument,
        }
    }
    unsafe fn addPoint(&self, _o: i32, _v: f64, _i: *mut i32) -> tresult {
        kResultFalse
    }
}

struct Changes {
    queues: Vec<ComWrapper<Queue>>,
}

impl Class for Changes {
    type Interfaces = (IParameterChanges,);
}

impl IParameterChangesTrait for Changes {
    unsafe fn getParameterCount(&self) -> i32 {
        self.queues.len() as i32
    }
    unsafe fn getParameterData(&self, index: i32) -> *mut IParamValueQueue {
        self.queues.get(index as usize).and_then(|q| q.as_com_ref::<IParamValueQueue>()).map(|r| r.as_ptr()).unwrap_or(std::ptr::null_mut())
    }
    unsafe fn addParameterData(&self, _id: *const ParamID, _index: *mut i32) -> *mut IParamValueQueue {
        std::ptr::null_mut()
    }
}

#[derive(Default)]
struct Handler {
    log: Mutex<Vec<(char, u32, f64)>>,
}

impl Class for Handler {
    type Interfaces = (IComponentHandler,);
}

impl IComponentHandlerTrait for Handler {
    unsafe fn beginEdit(&self, id: ParamID) -> tresult {
        self.log.lock().unwrap().push(('b', id, 0.0));
        kResultOk
    }
    unsafe fn performEdit(&self, id: ParamID, v: f64) -> tresult {
        self.log.lock().unwrap().push(('p', id, v));
        kResultOk
    }
    unsafe fn endEdit(&self, id: ParamID) -> tresult {
        self.log.lock().unwrap().push(('e', id, 0.0));
        kResultOk
    }
    unsafe fn restartComponent(&self, flags: i32) -> tresult {
        self.log.lock().unwrap().push(('r', 0, f64::from(flags)));
        kResultOk
    }
}

// ---- helpers -------------------------------------------------------------------------------

fn instance() -> ComPtr<IComponent> {
    let f = ComWrapper::new(Factory).to_com_ptr::<IPluginFactory>().unwrap();
    let mut cid = PLUMB_CID;
    let iid: TUID = <IComponent as vst3::Interface>::IID.map(|b| b as std::ffi::c_char);
    let mut obj: *mut c_void = std::ptr::null_mut();
    let r = unsafe { f.createInstance(cid.as_mut_ptr(), iid.as_ptr(), &mut obj) };
    assert_eq!(r, kResultOk);
    unsafe { ComPtr::from_raw(obj.cast::<IComponent>()) }.unwrap()
}

fn prepared(sr: f64, block: i32) -> (ComPtr<IComponent>, ComPtr<IAudioProcessor>, ComPtr<IEditController>) {
    let c = instance();
    let p = c.cast::<IAudioProcessor>().unwrap();
    let e = c.cast::<IEditController>().unwrap();
    unsafe {
        assert_eq!(c.initialize(std::ptr::null_mut()), kResultOk);
        let mut ins = [SpeakerArr::kStereo];
        let mut outs = [SpeakerArr::kStereo];
        assert_eq!(p.setBusArrangements(ins.as_mut_ptr(), 1, outs.as_mut_ptr(), 1), kResultTrue);
        let mut setup = ProcessSetup {
            processMode: ProcessModes_::kOffline as i32,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
            maxSamplesPerBlock: block,
            sampleRate: sr,
        };
        assert_eq!(p.setupProcessing(&mut setup), kResultOk);
        assert_eq!(c.setActive(1), kResultOk);
        assert_eq!(p.setProcessing(1), kResultOk);
    }
    (c, p, e)
}

/// Runs one block in place (`io` = left, right) with optional parameter changes.
fn run(p: &ComPtr<IAudioProcessor>, io: &mut [Vec<f32>; 2], changes: Option<&ComWrapper<Changes>>) {
    let frames = io[0].len();
    let mut chans: [*mut f32; 2] = [io[0].as_mut_ptr(), io[1].as_mut_ptr()];
    let mut ins: AudioBusBuffers = unsafe { std::mem::zeroed() };
    ins.numChannels = 2;
    ins.__field0.channelBuffers32 = chans.as_mut_ptr();
    let mut outs: AudioBusBuffers = unsafe { std::mem::zeroed() };
    outs.numChannels = 2;
    outs.__field0.channelBuffers32 = chans.as_mut_ptr();
    let mut d: ProcessData = unsafe { std::mem::zeroed() };
    d.processMode = ProcessModes_::kOffline as i32;
    d.symbolicSampleSize = SymbolicSampleSizes_::kSample32 as i32;
    d.numSamples = frames as i32;
    d.numInputs = 1;
    d.numOutputs = 1;
    d.inputs = &mut ins;
    d.outputs = &mut outs;
    d.inputParameterChanges = changes.and_then(|c| c.as_com_ref::<IParameterChanges>()).map(|r| r.as_ptr()).unwrap_or(std::ptr::null_mut());
    assert_eq!(unsafe { p.process(&mut d) }, kResultOk);
}

fn changes(list: &[(u32, &[(i32, f64)])]) -> ComWrapper<Changes> {
    ComWrapper::new(Changes { queues: list.iter().map(|(id, pts)| ComWrapper::new(Queue { id: *id, points: pts.to_vec() })).collect() })
}

fn set(e: &ComPtr<IEditController>, p: &ComPtr<IAudioProcessor>, id: u32, plain: f64) {
    let n = unsafe { e.plainParamToNormalized(id, plain) };
    unsafe { e.setParamNormalized(id, n) };
    let c = changes(&[(id, &[(0, n)])]);
    run(p, &mut [vec![0.0; 1], vec![0.0; 1]], Some(&c));
}

// ---- tests ---------------------------------------------------------------------------------

#[test]
fn factory_exports_one_audio_module() {
    let f = unsafe { ComPtr::from_raw(crate::GetPluginFactory()) }.unwrap();
    unsafe {
        let mut fi: PFactoryInfo = std::mem::zeroed();
        assert_eq!(f.getFactoryInfo(&mut fi), kResultOk);
        assert_eq!(c8(&fi.vendor), "SoundCraft");
        assert_eq!(f.countClasses(), 1);
        let mut ci: PClassInfo = std::mem::zeroed();
        assert_eq!(f.getClassInfo(0, &mut ci), kResultOk);
        assert_eq!((c8(&ci.category), c8(&ci.name)), ("Audio Module Class".into(), "Plumb".into()));
        assert_eq!(ci.cid, PLUMB_CID);
        assert_ne!(f.getClassInfo(1, &mut ci), kResultOk);
        let f2 = f.cast::<IPluginFactory2>().unwrap();
        let mut c2: PClassInfo2 = std::mem::zeroed();
        assert_eq!(f2.getClassInfo2(0, &mut c2), kResultOk);
        assert_eq!(c8(&c2.subCategories), "Fx|Pitch Shift");
        // An unknown class id is refused.
        let mut bad: TUID = [0; 16];
        let iid: TUID = <IComponent as vst3::Interface>::IID.map(|b| b as std::ffi::c_char);
        let mut obj: *mut c_void = std::ptr::null_mut();
        assert_ne!(f.createInstance(bad.as_mut_ptr(), iid.as_ptr(), &mut obj), kResultOk);
        assert!(obj.is_null());
    }
}

#[test]
fn parameters_have_titles_units_steps_and_ranges() {
    let (_c, _p, e) = prepared(48_000.0, 256);
    unsafe {
        assert_eq!(e.getParameterCount(), PARAM_COUNT as i32);
        let mut seen = Vec::new();
        for i in 0..PARAM_COUNT as i32 {
            let mut info: ParameterInfo = std::mem::zeroed();
            assert_eq!(e.getParameterInfo(i, &mut info), kResultOk);
            seen.push((info.id, c16(&info.title), c16(&info.units), info.stepCount));
            assert!((info.defaultNormalizedValue - params::default_normalized(info.id)).abs() < 1e-9);
            assert_eq!(info.flags & ParameterInfo_::ParameterFlags_::kIsBypass != 0, info.id == BYPASS);
            assert_eq!(info.flags & ParameterInfo_::ParameterFlags_::kIsList != 0, info.id == KEY || info.id == SCALE || info.id == MODE || info.id == params::SYNC);
        }
        let want: Vec<(u32, String, String, i32)> = [
            (0, "Key", "", 11),
            (1, "Scale", "", 12),
            (2, "Retune Speed", "ms", 0),
            (3, "Flex Tune", "%", 0),
            (4, "Reference", "Hz", 0),
            (5, "Mode", "", 1),
            (6, "Dampen", "%", 0),
            (7, "Bypass", "", 1),
            (8, "Sync", "", 5),
        ]
        .iter()
        .map(|&(i, t, u, s)| (i, t.to_string(), u.to_string(), s))
        .collect();
        assert_eq!(seen, want);
        let mut info: ParameterInfo = std::mem::zeroed();
        assert_ne!(e.getParameterInfo(9, &mut info), kResultOk);
        for id in [SPEED, FLEX, REFERENCE] {
            for n in [0.0, 0.25, 0.5, 1.0] {
                let back = e.plainParamToNormalized(id, e.normalizedParamToPlain(id, n));
                assert!((back - n).abs() < 1e-5, "id {id} {n} {back}");
            }
        }
        let mut s: String128 = [0; 128];
        assert_eq!(e.getParamStringByValue(SCALE, 1.0, &mut s), kResultOk);
        assert_eq!(c16(&s), "Blues");
        assert_eq!(e.getParamStringByValue(SPEED, 1.0, &mut s), kResultOk);
        assert_eq!(c16(&s), "Lock 3.0 ms");
        assert_eq!(e.getParamStringByValue(params::SYNC, 0.4, &mut s), kResultOk);
        assert_eq!(c16(&s), "Group 2");
        assert_eq!(e.getParamStringByValue(KEY, 1.0 / 11.0, &mut s), kResultOk);
        assert_eq!(c16(&s), "C#/Db");
        let mut text: Vec<TChar> = "432 Hz".encode_utf16().map(|c| c as TChar).chain(Some(0)).collect();
        let mut v = 0.0;
        assert_eq!(e.getParamValueByString(REFERENCE, text.as_mut_ptr(), &mut v), kResultOk);
        assert!((e.normalizedParamToPlain(REFERENCE, v) - 432.0).abs() < 0.01);
        assert_eq!(e.setParamNormalized(99, 0.5), kInvalidArgument);
    }
}

#[test]
fn state_save_load_round_trip() {
    let (c, _p, e) = prepared(48_000.0, 256);
    let values = [(KEY, 4.0 / 11.0), (SCALE, 0.5), (SPEED, 0.6), (FLEX, 0.3), (REFERENCE, 0.1), (MODE, 1.0), (BYPASS, 1.0)];
    unsafe {
        for (id, n) in values {
            e.setParamNormalized(id, n);
        }
        let s = ComWrapper::new(MemStream::default());
        let sp = s.as_com_ref::<IBStream>().unwrap().as_ptr();
        assert_eq!(c.getState(sp), kResultOk);
        let blob = s.data.borrow().clone();
        assert_eq!(&blob[..4], b"PLMB");
        assert_eq!(u32::from_le_bytes(blob[4..8].try_into().unwrap()), params::STATE_VERSION);

        let (c2, _p2, e2) = prepared(44_100.0, 128);
        *s.pos.borrow_mut() = 0;
        assert_eq!(c2.setState(sp), kResultOk);
        for id in 0..PARAM_COUNT as u32 {
            assert!((e2.getParamNormalized(id) - e.getParamNormalized(id)).abs() < 1e-6, "id {id}");
        }
        let junk = ComWrapper::new(MemStream { data: RefCell::new(b"not a state at all".to_vec()), pos: RefCell::new(0) });
        let before: Vec<f64> = (0..PARAM_COUNT as u32).map(|id| e2.getParamNormalized(id)).collect();
        assert_eq!(c2.setState(junk.as_com_ref::<IBStream>().unwrap().as_ptr()), kResultFalse);
        let after: Vec<f64> = (0..PARAM_COUNT as u32).map(|id| e2.getParamNormalized(id)).collect();
        assert_eq!(before, after);
    }
}

#[test]
fn silence_in_silence_out() {
    let (_c, p, e) = prepared(48_000.0, 512);
    for speed in [-3.0, 0.0, 100.0] {
        set(&e, &p, SPEED, speed);
        for _ in 0..8 {
            let mut io = [vec![0.0f32; 512], vec![0.0f32; 512]];
            run(&p, &mut io, None);
            assert!(io.iter().flatten().all(|&x| x == 0.0), "speed {speed}");
        }
    }
}

#[test]
fn a_sharp_sine_is_pulled_to_a4() {
    let (_c, p, e) = prepared(48_000.0, 512);
    set(&e, &p, SPEED, 0.0);
    let f = 440.0 * 2f32.powf(0.3 / 12.0);
    let mut out = Vec::new();
    for b in 0..100 {
        let x: Vec<f32> = (0..512).map(|i| 0.5 * (std::f32::consts::TAU * f * (b * 512 + i) as f32 / 48_000.0).sin()).collect();
        let mut io = [x.clone(), x];
        run(&p, &mut io, None);
        assert_eq!(io[0], io[1]);
        out.extend_from_slice(&io[0]);
    }
    let got = soundcraft_dsp::pitch_detect::yin(&out[30_000..32_048], 48_000.0, 60.0, 1600.0, 0.15).unwrap();
    assert!((1200.0 * (got / 440.0).log2()).abs() < 5.0, "{got} Hz");
    // A loud burst stays finite.
    let x: Vec<f32> = (0..512).map(|i| 4.0 * (i as f32 * 0.2).sin()).collect();
    let mut io = [x.clone(), x];
    run(&p, &mut io, None);
    assert!(io.iter().flatten().all(|y| y.is_finite() && y.abs() < 40.0));
}

#[test]
fn bypass_passes_the_input_through_delayed_by_the_latency() {
    let (_c, p, e) = prepared(48_000.0, 480);
    set(&e, &p, BYPASS, 1.0);
    let lat = unsafe { p.getLatencySamples() } as usize;
    assert_eq!(lat, 960);
    let x: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
    let mut out = Vec::new();
    for chunk in x.chunks(480) {
        let mut io = [chunk.to_vec(), chunk.to_vec()];
        run(&p, &mut io, None);
        out.extend_from_slice(&io[0]);
    }
    for i in 1500..4800 {
        assert!((out[i] - x[i - lat]).abs() < 1e-6, "{i}");
    }
}

#[test]
fn latency_follows_lock_mode_and_is_reported_to_the_host() {
    let plugin = ComWrapper::new(PlumbPlugin::new());
    let e = plugin.to_com_ptr::<IEditController>().unwrap();
    let p = plugin.to_com_ptr::<IAudioProcessor>().unwrap();
    let h = ComWrapper::new(Handler::default());
    unsafe {
        let mut setup = ProcessSetup {
            processMode: ProcessModes_::kOffline as i32,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
            maxSamplesPerBlock: 256,
            sampleRate: 48_000.0,
        };
        assert_eq!(p.setupProcessing(&mut setup), kResultOk);
        assert_eq!(e.setComponentHandler(h.as_com_ref::<IComponentHandler>().unwrap().as_ptr()), kResultOk);
        assert_eq!(p.getLatencySamples(), 960);
        assert_eq!(p.getTailSamples(), 960);
        e.setParamNormalized(SPEED, e.plainParamToNormalized(SPEED, -3.0));
        assert_eq!(p.getLatencySamples(), 2160 + 144);
        e.setParamNormalized(SPEED, e.plainParamToNormalized(SPEED, 50.0));
        assert_eq!(p.getLatencySamples(), 960);
        e.setParamNormalized(MODE, 1.0);
        assert_eq!(p.getLatencySamples(), 24, "Record: 0.5 ms");
        e.setParamNormalized(MODE, 0.0);
        assert_eq!(p.getLatencySamples(), 960);
    }
    let restarts = h.log.lock().unwrap().iter().filter(|(k, _, f)| *k == 'r' && *f as i32 == RestartFlags_::kLatencyChanged as i32).count();
    assert_eq!(restarts, 4, "one restart per latency change");
    // An automation point (audio thread) flags the change for the UI thread to send.
    let ch = changes(&[(SPEED, &[(0, unsafe { e.plainParamToNormalized(SPEED, -1.0) })])]);
    run(&p, &mut [vec![0.0; 64], vec![0.0; 64]], Some(&ch));
    assert_ne!(plugin.shared.restart.load(std::sync::atomic::Ordering::Acquire), 0);
    bentsignal_ui::host::flush_restart(&plugin.shared);
    assert_eq!(plugin.shared.restart.load(std::sync::atomic::Ordering::Acquire), 0);
    assert_eq!(unsafe { p.getLatencySamples() }, 2160 + 48);
    unsafe {
        assert_eq!(p.canProcessSampleSize(SymbolicSampleSizes_::kSample64 as i32), kResultFalse);
        let mut mono = [SpeakerArr::kMono];
        let mut st = [SpeakerArr::kStereo];
        assert_eq!(p.setBusArrangements(mono.as_mut_ptr(), 1, st.as_mut_ptr(), 1), kResultFalse);
    }
}

#[test]
fn input_meter_follows_the_input_and_falls() {
    let plugin = ComWrapper::new(PlumbPlugin::new());
    let pr = plugin.to_com_ptr::<IAudioProcessor>().unwrap();
    unsafe {
        let mut setup = ProcessSetup {
            processMode: ProcessModes_::kOffline as i32,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
            maxSamplesPerBlock: 480,
            sampleRate: 48_000.0,
        };
        assert_eq!(pr.setupProcessing(&mut setup), kResultOk);
    }
    let x: Vec<f32> = (0..480).map(|i| 0.5 * (i as f32 * 0.1).sin()).collect();
    run(&pr, &mut [x.clone(), x], None);
    let peak = plugin.shared.meter();
    assert!(peak > 0.45 && peak <= 0.5, "{peak}");
}

#[test]
fn editor_edits_reach_the_host_handler() {
    let plugin = ComWrapper::new(PlumbPlugin::new());
    let e = plugin.to_com_ptr::<IEditController>().unwrap();
    let h = ComWrapper::new(Handler::default());
    unsafe {
        assert_eq!(e.setComponentHandler(h.as_com_ref::<IComponentHandler>().unwrap().as_ptr()), kResultOk);
    }
    bentsignal_ui::host::edit(&plugin.shared, FLEX, 0.75);
    assert_eq!(*h.log.lock().unwrap(), vec![('b', FLEX, 0.0), ('p', FLEX, 0.75), ('e', FLEX, 0.0)]);
    assert_eq!(unsafe { e.getParamNormalized(FLEX) }, 0.75);
    unsafe {
        assert_eq!(e.terminate(), kResultOk);
    }
    bentsignal_ui::host::edit(&plugin.shared, FLEX, 0.1);
    assert_eq!(h.log.lock().unwrap().len(), 3, "no edits reach a released handler");
}
