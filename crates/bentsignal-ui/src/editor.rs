//! The editor: an `IPlugView` whose Win32 child window (inside the host's `HWND`) is drawn with
//! GDI, supersampled 2x and scaled down with halftone filtering for smooth knobs. Layout and hit
//! testing live in `layout`; values in `Shared` (the editor redraws when they change, polled on a
//! 30 Hz timer through the dirty flag). Edits go to the host through `IComponentHandler`
//! (`host`). One implementation serves every Bent Signal plugin; a `UiSpec` says what to show.

use crate::host::{begin_edit, edit, end_edit, flush_restart, perform_edit};
use crate::layout::{self, Hit, Knob, Layout, Rect};
use crate::paint;
use crate::shared::Shared;
use crate::spec::{BUNDLE_NAME, Glyph, SelectorSpec, Theme, UiSpec};
use std::cell::Cell;
use std::ffi::{CStr, c_void};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use vst3::Steinberg::*;
use vst3::{Class, ComRef, ComWrapper};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleHandleExW,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const TIMER_ID: usize = 1;
/// Timer ticks (33 ms) of hovering before the tooltip shows.
const TIP_TICKS: u32 = 14;
/// Supersampling factor of the drawing.
const SS: i32 = 2;
const MK_SHIFT: usize = 0x0004;
const MK_CONTROL: usize = 0x0008;
const WM_MOUSELEAVE: u32 = 0x02A3;

/// The size an editor of this plugin was last left at, for instances whose state has none.
static LAST_SIZE: AtomicU64 = AtomicU64::new(0);

/// Where a new editor opens: the size saved in the plugin state, else the last one used in this
/// process, else the base size.
fn initial_size(spec: &UiSpec, shared: &Shared) -> (i32, i32) {
    let last = LAST_SIZE.load(Ordering::Relaxed);
    let last = ((last >> 32) as i32, (last & 0xFFFF_FFFF) as i32);
    let (w, h) = shared.saved_size().or((last.0 > 0 && last.1 > 0).then_some(last)).unwrap_or_else(|| layout::default_size(spec));
    layout::clamp_size(spec, w, h)
}

/// Remembers the editor size (in the plugin state and for the next editor opened).
fn remember_size(shared: &Shared, w: i32, h: i32) {
    shared.save_size(w, h);
    if w > 0 && h > 0 {
        LAST_SIZE.store((u64::from(w as u32) << 32) | u64::from(h as u32), Ordering::Relaxed);
    }
}

/// Window classes registered by this module (one per plugin spec).
static CLASSES: Mutex<Vec<String>> = Mutex::new(Vec::new());

const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    r as u32 | ((g as u32) << 8) | ((b as u32) << 16)
}

/// The editor's colours, one set per theme (chosen when drawing starts).
#[derive(Clone, Copy)]
struct Pal {
    panel: u32,
    bar: u32,
    edge: u32,
    field: u32,
    btn: u32,
    btn_edge: u32,
    rust: u32,
    rust_dim: u32,
    text: u32,
    dim: u32,
    dial: u32,
    track: u32,
    pointer: u32,
    tip_bg: u32,
    /// Outline of a dial.
    outline: u32,
    inner_ring: u32,
    pointer2: u32,
}

// Walnut: flat, matte charcoal brown-black panel, one warm rust accent.
const WALNUT: Pal = Pal {
    panel: rgb(0x1C, 0x19, 0x17),
    bar: rgb(0x27, 0x23, 0x20),
    edge: rgb(0x0D, 0x0B, 0x0A),
    field: rgb(0x12, 0x10, 0x0F),
    btn: rgb(0x3A, 0x35, 0x31),
    btn_edge: rgb(0x58, 0x51, 0x4A),
    rust: rgb(0xB8, 0x5C, 0x32),
    rust_dim: rgb(0x6B, 0x3B, 0x24),
    text: rgb(0xDA, 0xD2, 0xC6),
    dim: rgb(0x8E, 0x86, 0x7C),
    dial: rgb(0x2C, 0x28, 0x25),
    track: rgb(0x3E, 0x38, 0x33),
    pointer: rgb(0xEE, 0xE8, 0xDE),
    tip_bg: rgb(0x0F, 0x0D, 0x0C),
    outline: rgb(0x0D, 0x0B, 0x0A),
    inner_ring: rgb(0x34, 0x2F, 0x2B),
    pointer2: rgb(0xC8, 0xC4, 0xBE),
};

// Grey: dark grey panel, controls a step lighter, dials lighter still, outlines brighter than any
// fill.
const GREY: Pal = Pal {
    panel: rgb(0x26, 0x28, 0x2B),
    bar: rgb(0x2E, 0x30, 0x34),
    edge: rgb(0x17, 0x18, 0x1A),
    field: rgb(0x1D, 0x1E, 0x21),
    btn: rgb(0x33, 0x36, 0x3B),
    btn_edge: rgb(0x6A, 0x6E, 0x77),
    rust: rgb(0xB8, 0x5C, 0x32),
    rust_dim: rgb(0x6B, 0x3B, 0x24),
    text: rgb(0xD9, 0xDB, 0xDE),
    dim: rgb(0x8D, 0x91, 0x99),
    dial: rgb(0x3C, 0x3F, 0x45),
    track: rgb(0x34, 0x37, 0x3D),
    pointer: rgb(0xF0, 0xF1, 0xF3),
    tip_bg: rgb(0x1A, 0x1B, 0x1E),
    outline: rgb(0x72, 0x76, 0x7F),
    inner_ring: rgb(0x4B, 0x4E, 0x55),
    pointer2: rgb(0xCF, 0xD2, 0xD6),
};

thread_local! {
    static PAL: Cell<Pal> = const { Cell::new(WALNUT) };
}

/// The palette in use (set by `draw` from the spec's theme).
fn p() -> Pal {
    PAL.with(Cell::get)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn module() -> *mut c_void {
    let mut h: *mut c_void = std::ptr::null_mut();
    // SAFETY: looks up the module containing this function without changing its reference count.
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            wndproc as *const () as *const u16,
            &mut h,
        );
    }
    h
}

fn register_class(spec: &UiSpec) -> bool {
    let Ok(mut classes) = CLASSES.lock() else { return false };
    if classes.iter().any(|c| c == spec.class_name) {
        return true;
    }
    let name = wide(spec.class_name);
    // SAFETY: a fully initialised class description; `name` outlives the call.
    let ok = unsafe {
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: module(),
            hIcon: std::ptr::null_mut(),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: name.as_ptr(),
            hIconSm: std::ptr::null_mut(),
        };
        RegisterClassExW(&wc) != 0
    };
    if ok {
        classes.push(spec.class_name.to_string());
    }
    ok
}

/// Unregisters the window classes (module exit).
pub fn unregister_class() {
    let Ok(mut classes) = CLASSES.lock() else { return };
    for c in classes.drain(..) {
        let name = wide(&c);
        // SAFETY: our class, registered by this module.
        unsafe { UnregisterClassW(name.as_ptr(), module()) };
    }
}

// ---- the view ------------------------------------------------------------------------------

struct View {
    spec: &'static UiSpec,
    shared: Arc<Shared>,
    hwnd: Cell<HWND>,
    /// The host's IPlugFrame (not AddRef'd: the host keeps it alive while set), 0 when none.
    frame: Rc<Cell<usize>>,
    /// Our own IPlugView pointer (for `resizeView`), not AddRef'd.
    me: Rc<Cell<usize>>,
    size: Rc<Cell<(i32, i32)>>,
}

impl Class for View {
    type Interfaces = (IPlugView,);
}

/// A new editor view for the controller's values, as an owned `IPlugView` pointer.
pub fn create_view(shared: Arc<Shared>, spec: &'static UiSpec) -> *mut IPlugView {
    let size = initial_size(spec, &shared);
    let view = View {
        spec,
        shared,
        hwnd: Cell::new(std::ptr::null_mut()),
        frame: Rc::new(Cell::new(0)),
        me: Rc::new(Cell::new(0)),
        size: Rc::new(Cell::new(size)),
    };
    let me = view.me.clone();
    match ComWrapper::new(view).to_com_ptr::<IPlugView>() {
        Some(p) => {
            let raw = p.into_raw();
            me.set(raw as usize);
            raw
        }
        None => std::ptr::null_mut(),
    }
}

impl View {
    fn destroy_window(&self) {
        let h = self.hwnd.replace(std::ptr::null_mut());
        if !h.is_null() {
            // SAFETY: our child window; WM_NCDESTROY frees its state.
            unsafe { DestroyWindow(h) };
        }
    }
}

impl Drop for View {
    fn drop(&mut self) {
        self.destroy_window();
    }
}

impl IPlugViewTrait for View {
    unsafe fn isPlatformTypeSupported(&self, t: FIDString) -> tresult {
        if t.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: a null-terminated type string from the host.
        if unsafe { CStr::from_ptr(t) }.to_bytes() == b"HWND" { kResultTrue } else { kResultFalse }
    }
    unsafe fn attached(&self, parent: *mut c_void, t: FIDString) -> tresult {
        // SAFETY: as above.
        if parent.is_null() || t.is_null() || unsafe { CStr::from_ptr(t) }.to_bytes() != b"HWND" {
            return kInvalidArgument;
        }
        if !register_class(self.spec) {
            return kResultFalse;
        }
        self.destroy_window();
        let (w, h) = self.size.get();
        let class = wide(self.spec.class_name);
        let title = wide(self.spec.title);
        // SAFETY: a child of the host's live window, our registered class.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
                0,
                0,
                w,
                h,
                parent,
                std::ptr::null_mut(),
                module(),
                std::ptr::null(),
            )
        };
        if hwnd.is_null() {
            return kResultFalse;
        }
        let win = Box::new(Win {
            typing: None,
            spec: self.spec,
            shared: self.shared.clone(),
            frame: self.frame.clone(),
            me: self.me.clone(),
            size: self.size.clone(),
            drag: None,
            hover: None,
            hover_at: (0, 0),
            hover_ticks: 0,
            tip: false,
            tracking: false,
            user: None,
            last_preset: 0,
            last_meter: 0,
            update: Default::default(),
        });
        let slot = win.update.clone();
        // SAFETY: our window; the box is reclaimed in WM_NCDESTROY.
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(win) as isize);
            SetTimer(hwnd, TIMER_ID, 33, None);
            InvalidateRect(hwnd, std::ptr::null(), 0);
        }
        self.hwnd.set(hwnd);
        crate::update_win::spawn_check(hwnd as usize, slot);
        kResultOk
    }
    unsafe fn removed(&self) -> tresult {
        self.destroy_window();
        kResultOk
    }
    unsafe fn onWheel(&self, _d: f32) -> tresult {
        kResultFalse
    }
    unsafe fn onKeyDown(&self, _k: char16, _c: i16, _m: i16) -> tresult {
        kResultFalse
    }
    unsafe fn onKeyUp(&self, _k: char16, _c: i16, _m: i16) -> tresult {
        kResultFalse
    }
    unsafe fn getSize(&self, size: *mut ViewRect) -> tresult {
        // SAFETY: the host passes a writable rect or null.
        let Some(r) = (unsafe { size.as_mut() }) else { return kInvalidArgument };
        let (w, h) = self.size.get();
        *r = ViewRect { left: 0, top: 0, right: w, bottom: h };
        kResultOk
    }
    unsafe fn onSize(&self, size: *mut ViewRect) -> tresult {
        // SAFETY: the host passes a valid rect or null.
        let Some(r) = (unsafe { size.as_ref() }) else { return kInvalidArgument };
        let (w, h) = ((r.right - r.left).max(1), (r.bottom - r.top).max(1));
        self.size.set((w, h));
        remember_size(&self.shared, w, h);
        let hwnd = self.hwnd.get();
        if !hwnd.is_null() {
            // SAFETY: our live child window.
            unsafe {
                SetWindowPos(hwnd, std::ptr::null_mut(), 0, 0, w, h, SWP_NOZORDER | SWP_NOMOVE | SWP_NOACTIVATE);
                InvalidateRect(hwnd, std::ptr::null(), 0);
            }
        }
        kResultOk
    }
    unsafe fn onFocus(&self, _s: TBool) -> tresult {
        kResultOk
    }
    unsafe fn setFrame(&self, frame: *mut IPlugFrame) -> tresult {
        self.frame.set(frame as usize);
        kResultOk
    }
    unsafe fn canResize(&self) -> tresult {
        // False: the host adds no sizing border (resizable from every edge); the corner grip
        // resizes through `resizeView`, which hosts honour either way.
        kResultFalse
    }
    unsafe fn checkSizeConstraint(&self, rect: *mut ViewRect) -> tresult {
        // SAFETY: the host passes a writable rect or null.
        let Some(r) = (unsafe { rect.as_mut() }) else { return kInvalidArgument };
        let (w, h) = layout::clamp_size(self.spec, r.right - r.left, r.bottom - r.top);
        r.right = r.left + w;
        r.bottom = r.top + h;
        kResultTrue
    }
}

// ---- the window ----------------------------------------------------------------------------

enum Drag {
    Knob { id: u32, y0: i32, start: f64 },
    Grip { x0: i32, y0: i32, w0: i32, h0: i32 },
}

/// A number box being typed into.
struct Typing {
    index: usize,
    text: String,
    /// Nothing typed yet: the first character replaces the shown value.
    fresh: bool,
}

pub struct Win {
    typing: Option<Typing>,
    spec: &'static UiSpec,
    shared: Arc<Shared>,
    frame: Rc<Cell<usize>>,
    me: Rc<Cell<usize>>,
    size: Rc<Cell<(i32, i32)>>,
    drag: Option<Drag>,
    hover: Option<Hit>,
    hover_at: (i32, i32),
    hover_ticks: u32,
    tip: bool,
    tracking: bool,
    /// The User preset from Save (this editor session only).
    user: Option<Vec<f64>>,
    /// The preset the arrows step from when the values match none.
    last_preset: usize,
    /// The meter level (bits) last drawn.
    last_meter: u32,
    /// Where the update-check thread leaves a release to ask about.
    update: crate::update_win::Slot,
}

fn client_size(hwnd: HWND) -> (i32, i32) {
    let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    // SAFETY: our window; `r` is writable.
    unsafe { GetClientRect(hwnd, &mut r) };
    ((r.right - r.left).max(1), (r.bottom - r.top).max(1))
}

fn invalidate(hwnd: HWND) {
    // SAFETY: our window.
    unsafe { InvalidateRect(hwnd, std::ptr::null(), 0) };
}

fn xy(lp: LPARAM) -> (i32, i32) {
    ((lp & 0xFFFF) as u16 as i16 as i32, ((lp >> 16) & 0xFFFF) as u16 as i16 as i32)
}

/// The preset whose values the current ones match (factory first, then User = `presets.len()`).
pub fn matching_preset(spec: &UiSpec, values: &[f64], user: Option<&Vec<f64>>) -> Option<usize> {
    let near = |id: usize, n: f64| values.get(id).is_some_and(|v| (v - n).abs() < 1e-4);
    spec.presets
        .iter()
        .position(|p| p.values.iter().enumerate().all(|(id, v)| near(id, spec.model.to_normalized(id as u32, *v))))
        .or_else(|| user.filter(|u| u.iter().take(spec.model.dsp_count()).enumerate().all(|(id, v)| near(id, *v))).map(|_| spec.presets.len()))
}

fn apply_values(shared: &Shared, normalized: &[f64]) {
    for (id, n) in normalized.iter().enumerate().take(shared.model().dsp_count()) {
        edit(shared, id as u32, *n);
    }
}

fn apply_preset(w: &mut Win, index: usize) {
    if let Some(p) = w.spec.presets.get(index) {
        let n: Vec<f64> = p.values.iter().enumerate().map(|(id, v)| w.spec.model.to_normalized(id as u32, *v)).collect();
        apply_values(&w.shared, &n);
        w.last_preset = index;
    } else if let Some(u) = &w.user {
        apply_values(&w.shared, u);
    }
}

/// Shows a popup menu of `items` (checked index) under `at` and returns the choice.
fn popup(hwnd: HWND, at: &Rect, items: &[&str], checked: Option<usize>) -> Option<usize> {
    // SAFETY: a popup menu created, shown modally on our live window and destroyed here.
    unsafe {
        let m = CreatePopupMenu();
        if m.is_null() {
            return None;
        }
        for (i, item) in items.iter().enumerate() {
            let flags = if checked == Some(i) { MF_STRING | MF_CHECKED } else { MF_STRING };
            let text = wide(item);
            AppendMenuW(m, flags, i + 1, text.as_ptr());
        }
        let mut p = POINT { x: at.left, y: at.bottom };
        ClientToScreen(hwnd, &mut p);
        let id = TrackPopupMenu(m, TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN, p.x, p.y, 0, hwnd, std::ptr::null());
        DestroyMenu(m);
        (id > 0).then(|| id as usize - 1)
    }
}

/// For a click on a dropdown: its index, where to show the menu, the items and the checked one.
fn dropdown_menu(hwnd: HWND, w: &mut Win, x: i32, y: i32) -> Option<(usize, Rect, Vec<&'static str>, Option<usize>)> {
    let (cw, ch) = client_size(hwnd);
    let l = layout::layout(w.spec, cw, ch);
    let Some(Hit::Dropdown(i)) = layout::hit(&l, x, y) else { return None };
    let (d, ds) = (l.dropdowns.get(i)?, w.spec.dropdowns.get(i)?);
    w.tip = false;
    let cur = w.spec.model.to_plain(ds.id, w.shared.get(ds.id)).round().max(0.0) as usize;
    Some((i, d.rect, ds.options.to_vec(), Some(cur)))
}

/// For a click on the preset name: where to show the preset menu, its items and the checked one.
fn preset_menu(hwnd: HWND, w: &mut Win, x: i32, y: i32) -> Option<(Rect, Vec<String>, Option<usize>)> {
    let (cw, ch) = client_size(hwnd);
    let l = layout::layout(w.spec, cw, ch);
    if layout::hit(&l, x, y) != Some(Hit::PresetName) {
        return None;
    }
    w.tip = false;
    let mut names: Vec<String> = w.spec.presets.iter().map(|p| p.name.to_string()).collect();
    if w.user.is_some() {
        names.push("User".to_string());
    }
    Some((l.name, names, matching_preset(w.spec, &w.shared.snapshot(), w.user.as_ref())))
}

fn click(hwnd: HWND, w: &mut Win, x: i32, y: i32, keys: usize, double: bool) {
    let (cw, ch) = client_size(hwnd);
    let spec = w.spec;
    let l = layout::layout(spec, cw, ch);
    let s = &w.shared;
    w.tip = false;
    match layout::hit(&l, x, y) {
        Some(Hit::Knob(id)) => {
            if double || keys & MK_CONTROL != 0 {
                edit(s, id, spec.model.default_normalized(id));
            } else {
                // SAFETY: our window.
                unsafe { SetCapture(hwnd) };
                begin_edit(s, id);
                w.drag = Some(Drag::Knob { id, y0: y, start: s.get(id) });
            }
        }
        Some(Hit::Option(i)) => {
            if let Some(sel) = spec.selector {
                edit(s, sel.id, spec.model.to_normalized(sel.id, i as f64));
            }
        }
        Some(Hit::Radio(i)) => {
            if let Some(q) = spec.radios.get(i) {
                let on = (spec.model.to_plain(q.id, s.get(q.id)) - q.value).abs() < 0.5;
                let v = if on { q.off_value.unwrap_or(q.value) } else { q.value };
                edit(s, q.id, spec.model.to_normalized(q.id, v));
            }
        }
        Some(Hit::Number(i)) => {
            if let Some(n) = spec.numbers.get(i) {
                let plain = spec.model.to_plain(n.id, s.get(n.id));
                let text = format!("{plain:.1}");
                w.typing = Some(Typing { index: i, text, fresh: true });
                // SAFETY: our window; keyboard focus lets it receive the typed characters.
                unsafe { SetFocus(hwnd) };
            }
        }
        // The dropdown menu is modal: `wndproc` shows it.
        Some(Hit::Dropdown(_)) => {}
        Some(Hit::Toggle(i)) => {
            if let Some(t) = spec.toggles.get(i) {
                edit(s, t.id, if s.get(t.id) >= 0.5 { 0.0 } else { 1.0 });
            }
        }
        Some(Hit::Bypass) => edit(s, spec.bypass_id, if s.get(spec.bypass_id) >= 0.5 { 0.0 } else { 1.0 }),
        Some(Hit::Meter) => {}
        Some(Hit::Reset) => apply_preset(w, 0),
        Some(Hit::Save) => {
            w.user = Some(s.snapshot());
            s.dirty.store(true, Ordering::Relaxed);
        }
        Some(Hit::PrevPreset) | Some(Hit::NextPreset) => {
            let count = spec.presets.len() + usize::from(w.user.is_some());
            let cur = matching_preset(spec, &s.snapshot(), w.user.as_ref()).unwrap_or(w.last_preset);
            let next = if layout::hit(&l, x, y) == Some(Hit::NextPreset) { (cur + 1) % count } else { (cur + count - 1) % count };
            apply_preset(w, next);
        }
        // The preset menu is modal: `wndproc` shows it without holding the window state.
        Some(Hit::PresetName) => {}
        Some(Hit::Grip) => {
            let mut p = POINT { x: 0, y: 0 };
            // SAFETY: our window; `p` is writable.
            unsafe {
                GetCursorPos(&mut p);
                SetCapture(hwnd);
            }
            w.drag = Some(Drag::Grip { x0: p.x, y0: p.y, w0: cw, h0: ch });
        }
        None => {}
    }
    invalidate(hwnd);
}

fn mouse_move(hwnd: HWND, w: &mut Win, x: i32, y: i32, keys: usize) {
    if !w.tracking {
        let mut t = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
        // SAFETY: our window; `t` is initialised.
        unsafe { TrackMouseEvent(&mut t) };
        w.tracking = true;
    }
    match &w.drag {
        Some(Drag::Knob { id, y0, start }) => {
            let (cw, ch) = client_size(hwnd);
            let n = layout::drag_value(*start, y0 - y, layout::layout(w.spec, cw, ch).scale, keys & MK_SHIFT != 0);
            perform_edit(&w.shared, *id, n);
            invalidate(hwnd);
        }
        Some(Drag::Grip { x0, y0, w0, h0 }) => {
            let mut p = POINT { x: 0, y: 0 };
            // SAFETY: `p` is writable.
            unsafe { GetCursorPos(&mut p) };
            let (nw, nh) = layout::clamp_size(w.spec, w0 + p.x - x0, h0 + p.y - y0);
            resize(hwnd, w, nw, nh);
        }
        None => {
            let (cw, ch) = client_size(hwnd);
            let h = layout::hit(&layout::layout(w.spec, cw, ch), x, y);
            if h != w.hover {
                w.hover = h;
                w.hover_ticks = 0;
                if w.tip {
                    w.tip = false;
                }
                invalidate(hwnd);
            }
            w.hover_at = (x, y);
        }
    }
}

/// Asks the host to resize the view (it calls `onSize` back); without a frame, resizes itself.
fn resize(hwnd: HWND, w: &Win, nw: i32, nh: i32) {
    if w.size.get() == (nw, nh) {
        return;
    }
    let mut r = ViewRect { left: 0, top: 0, right: nw, bottom: nh };
    // SAFETY: the frame the host set (alive while set) and our own view pointer.
    let asked = match unsafe { ComRef::from_raw(w.frame.get() as *mut IPlugFrame) } {
        Some(f) => (unsafe { f.resizeView(w.me.get() as *mut IPlugView, &mut r) }) == kResultOk,
        None => false,
    };
    if !asked {
        w.size.set((nw, nh));
        remember_size(&w.shared, nw, nh);
        // SAFETY: our window.
        unsafe { SetWindowPos(hwnd, std::ptr::null_mut(), 0, 0, nw, nh, SWP_NOZORDER | SWP_NOMOVE | SWP_NOACTIVATE) };
    }
    invalidate(hwnd);
}

fn end_drag(w: &mut Win) {
    if let Some(Drag::Knob { id, .. }) = w.drag.take() {
        end_edit(&w.shared, id);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: the user data is null or our `Win` box, owned by this window until WM_NCDESTROY;
    // messages for one window only arrive on its own thread.
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut Win;
    if ptr.is_null() {
        // SAFETY: default handling for a window without state yet.
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    }
    if matches!(msg, WM_LBUTTONDOWN | WM_LBUTTONDBLCLK) {
        {
            let (x, y) = xy(lp);
            let (cw, ch) = client_size(hwnd);
            // SAFETY: see above; short borrow.
            let w = unsafe { &mut *ptr };
            let on_box = layout::hit(&layout::layout(w.spec, cw, ch), x, y);
            if w.typing.as_ref().is_some_and(|t| on_box != Some(Hit::Number(t.index))) {
                finish_typing(w, true);
                invalidate(hwnd);
            }
        }
        let (x, y) = xy(lp);
        // SAFETY: see above; this borrow ends before the modal menu below can re-enter.
        let menu = preset_menu(hwnd, unsafe { &mut *ptr }, x, y);
        match menu {
            Some((at, names, cur)) => {
                let names: Vec<&str> = names.iter().map(String::as_str).collect();
                if let Some(i) = popup(hwnd, &at, &names, cur) {
                    // SAFETY: the menu is closed; nothing else borrows the state.
                    apply_preset(unsafe { &mut *ptr }, i);
                }
                invalidate(hwnd);
            }
            // SAFETY: as above.
            None => {
                // SAFETY: this borrow ends before the modal menu below can re-enter.
                match dropdown_menu(hwnd, unsafe { &mut *ptr }, x, y) {
                    Some((i, at, names, cur)) => {
                        if let Some(c) = popup(hwnd, &at, &names, cur) {
                            // SAFETY: the menu is closed; nothing else borrows the state.
                            let w = unsafe { &*ptr };
                            if let Some(d) = w.spec.dropdowns.get(i) {
                                edit(&w.shared, d.id, w.spec.model.to_normalized(d.id, c as f64));
                            }
                        }
                        invalidate(hwnd);
                    }
                    // SAFETY: as above.
                    None => click(hwnd, unsafe { &mut *ptr }, x, y, wp, msg == WM_LBUTTONDBLCLK),
                }
            }
        }
        return 0;
    }
    if msg == crate::update_win::WM_UPDATE_READY {
        // SAFETY: short borrow; the modal box below can re-enter, or let the host destroy the window.
        let (slot, title) = {
            let w = unsafe { &*ptr };
            (w.update.clone(), w.spec.title)
        };
        crate::update_win::on_update_ready(hwnd as usize, &slot, title);
        return 0;
    }
    // SAFETY: as above (no handler below re-enters the window procedure).
    let w = unsafe { &mut *ptr };
    match msg {
        WM_GETDLGCODE if w.typing.is_some() => (DLGC_WANTCHARS | DLGC_WANTALLKEYS) as LRESULT,
        WM_CHAR => {
            type_char(hwnd, w, wp as u32);
            0
        }
        // Typing ends on Enter, Esc or a click elsewhere, not on focus loss: a scaling host moves
        // the focus around its own windows right after the click and would drop the typed text.
        WM_KILLFOCUS => 0,
        WM_TIMER => {
            flush_restart(&w.shared);
            let meter = w.shared.meter().to_bits();
            if w.shared.dirty.swap(false, Ordering::Relaxed) || (w.spec.meter.is_some() && meter != w.last_meter) {
                invalidate(hwnd);
            }
            if w.hover.is_some() && !w.tip && w.drag.is_none() {
                w.hover_ticks += 1;
                if w.hover_ticks >= TIP_TICKS {
                    w.tip = true;
                    invalidate(hwnd);
                }
            }
            0
        }
        WM_MOUSEMOVE => {
            let (x, y) = xy(lp);
            mouse_move(hwnd, w, x, y, wp);
            0
        }
        WM_MOUSELEAVE => {
            w.tracking = false;
            if w.drag.is_none() {
                w.hover = None;
                w.tip = false;
                invalidate(hwnd);
            }
            0
        }

        WM_LBUTTONUP => {
            end_drag(w);
            // SAFETY: releases our own capture (WM_CAPTURECHANGED then finds no drag).
            unsafe { ReleaseCapture() };
            invalidate(hwnd);
            0
        }
        WM_CAPTURECHANGED => {
            end_drag(w);
            0
        }
        WM_SETCURSOR => {
            if w.hover == Some(Hit::Grip) || matches!(w.drag, Some(Drag::Grip { .. })) {
                // SAFETY: a system cursor.
                unsafe { SetCursor(LoadCursorW(std::ptr::null_mut(), IDC_SIZENWSE)) };
                return 1;
            }
            // SAFETY: default handling.
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        WM_ERASEBKGND => 1,
        WM_SIZE => {
            invalidate(hwnd);
            0
        }
        WM_PAINT => {
            // SAFETY: a zeroed PAINTSTRUCT is valid input to BeginPaint.
            let mut ps: PAINTSTRUCT = unsafe { std::mem::zeroed() };
            // SAFETY: WM_PAINT of our window; ended below.
            let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
            if !hdc.is_null() {
                paint(hwnd, hdc, w);
            }
            // SAFETY: pairs with BeginPaint.
            unsafe { EndPaint(hwnd, &ps) };
            0
        }
        WM_PRINT | WM_PRINTCLIENT => {
            paint(hwnd, wp as HDC, w);
            0
        }
        WM_NCDESTROY => {
            end_drag(w);
            // SAFETY: our window; reclaims the box stored in `attached` exactly once.
            unsafe {
                KillTimer(hwnd, TIMER_ID);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(w as *mut Win));
                DefWindowProcW(hwnd, msg, wp, lp)
            }
        }
        // SAFETY: default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

// ---- drawing -------------------------------------------------------------------------------

struct Fonts {
    word: HFONT,
    label: HFONT,
    small: HFONT,
    name: HFONT,
}

fn font(px: i32, weight: i32, face: &str) -> HFONT {
    let f = wide(face);
    // SAFETY: plain font creation; deleted by `Fonts::drop`.
    unsafe {
        CreateFontW(
            -px.max(1),
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            CLEARTYPE_QUALITY as u32,
            0,
            f.as_ptr(),
        )
    }
}

impl Fonts {
    fn new(scale: f64) -> Fonts {
        let px = |v: f64| (v * scale).round() as i32;
        Fonts {
            word: font(px(27.0), FW_SEMIBOLD as i32, "Bahnschrift"),
            label: font(px(12.5), FW_SEMIBOLD as i32, "Bahnschrift"),
            small: font(px(10.5), FW_NORMAL as i32, "Bahnschrift"),
            name: font(px(13.0), FW_NORMAL as i32, "Bahnschrift"),
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        for f in [self.word, self.label, self.small, self.name] {
            // SAFETY: our fonts, no longer selected.
            unsafe { DeleteObject(f) };
        }
    }
}

struct Gdi {
    hdc: HDC,
}

impl Gdi {
    fn fill(&self, r: &Rect, color: u32) {
        let rc = RECT { left: r.left, top: r.top, right: r.right, bottom: r.bottom };
        // SAFETY: GDI on our memory DC; the brush is deleted here.
        unsafe {
            let b = CreateSolidBrush(color);
            FillRect(self.hdc, &rc, b);
            DeleteObject(b);
        }
    }

    /// A filled rectangle with a 1-unit (scaled) border.
    fn boxed(&self, r: &Rect, fill: u32, edge: u32, t: i32) {
        self.fill(r, edge);
        self.fill(&Rect::new(r.left + t, r.top + t, r.right - t, r.bottom - t), fill);
    }

    fn polyline(&self, pts: &[(i32, i32)], color: u32, width: i32) {
        let p: Vec<POINT> = pts.iter().map(|&(x, y)| POINT { x, y }).collect();
        // SAFETY: GDI on our memory DC; the pen is selected out and deleted here.
        unsafe {
            let pen = CreatePen(PS_SOLID, width.max(1), color);
            let old = SelectObject(self.hdc, pen);
            Polyline(self.hdc, p.as_ptr(), p.len() as i32);
            SelectObject(self.hdc, old);
            DeleteObject(pen);
        }
    }

    fn polygon(&self, pts: &[(i32, i32)], color: u32) {
        let p: Vec<POINT> = pts.iter().map(|&(x, y)| POINT { x, y }).collect();
        // SAFETY: as above; the brush and pen are selected out and deleted here.
        unsafe {
            let b = CreateSolidBrush(color);
            let ob = SelectObject(self.hdc, b);
            let op = SelectObject(self.hdc, GetStockObject(NULL_PEN));
            Polygon(self.hdc, p.as_ptr(), p.len() as i32);
            SelectObject(self.hdc, op);
            SelectObject(self.hdc, ob);
            DeleteObject(b);
        }
    }

    fn ellipse(&self, cx: i32, cy: i32, r: i32, fill: u32, edge: u32, width: i32) {
        // SAFETY: as above.
        unsafe {
            let b = CreateSolidBrush(fill);
            let pen = CreatePen(PS_SOLID, width.max(1), edge);
            let ob = SelectObject(self.hdc, b);
            let op = SelectObject(self.hdc, pen);
            Ellipse(self.hdc, cx - r, cy - r, cx + r, cy + r);
            SelectObject(self.hdc, op);
            SelectObject(self.hdc, ob);
            DeleteObject(b);
            DeleteObject(pen);
        }
    }

    /// A walnut veneer strip: lines run along its length (`vertical` strips run top to bottom).
    fn wood(&self, r: &Rect, vertical: bool, seed: u32) {
        let (len, thick) = if vertical { (r.height(), r.width()) } else { (r.width(), r.height()) };
        for line in 0..thick.max(0) as usize {
            for (a, b, c) in paint::wood_runs(seed, line, thick as usize) {
                let (s, e) = ((a * len as f32).round() as i32, (b * len as f32).round() as i32);
                let q = line as i32;
                let rc = if vertical {
                    Rect::new(r.left + q, r.top + s, r.left + q + 1, r.top + e)
                } else {
                    Rect::new(r.left + s, r.top + q, r.left + e, r.top + q + 1)
                };
                self.fill(&rc, rgb(c[0], c[1], c[2]));
            }
        }
    }

    fn text(&self, r: &Rect, s: &str, f: HFONT, color: u32, flags: u32) {
        let mut w: Vec<u16> = s.encode_utf16().collect();
        let mut rc = RECT { left: r.left, top: r.top, right: r.right, bottom: r.bottom };
        // SAFETY: GDI on our memory DC; the font is selected back out.
        unsafe {
            let old = SelectObject(self.hdc, f);
            SetTextColor(self.hdc, color);
            DrawTextW(self.hdc, w.as_mut_ptr(), w.len() as i32, &mut rc, flags);
            SelectObject(self.hdc, old);
        }
    }

    /// The size `s` takes when word-wrapped to `max_w`.
    fn measure(&self, s: &str, f: HFONT, max_w: i32) -> (i32, i32) {
        let mut w: Vec<u16> = s.encode_utf16().collect();
        let mut rc = RECT { left: 0, top: 0, right: max_w, bottom: 0 };
        // SAFETY: as above.
        unsafe {
            let old = SelectObject(self.hdc, f);
            DrawTextW(self.hdc, w.as_mut_ptr(), w.len() as i32, &mut rc, DT_CALCRECT | DT_WORDBREAK | DT_LEFT);
            SelectObject(self.hdc, old);
        }
        (rc.right - rc.left, rc.bottom - rc.top)
    }
}

const CENTER: u32 = DT_CENTER | DT_VCENTER | DT_SINGLELINE;

fn paint(hwnd: HWND, target: HDC, w: &Win) {
    let (cw, ch) = client_size(hwnd);
    let (bw, bh) = (cw * SS, ch * SS);
    // SAFETY: an off-screen bitmap the size of the supersampled client, drawn into, scaled onto
    // `target` and freed here.
    unsafe {
        let mem = CreateCompatibleDC(target);
        let bmp = CreateCompatibleBitmap(target, bw, bh);
        if mem.is_null() || bmp.is_null() {
            if !bmp.is_null() {
                DeleteObject(bmp);
            }
            if !mem.is_null() {
                DeleteDC(mem);
            }
            return;
        }
        let old = SelectObject(mem, bmp);
        SetBkMode(mem, TRANSPARENT as i32);
        draw(&Gdi { hdc: mem }, &layout::layout(w.spec, bw, bh), w);
        SetStretchBltMode(target, HALFTONE);
        SetBrushOrgEx(target, 0, 0, std::ptr::null_mut());
        StretchBlt(target, 0, 0, cw, ch, mem, 0, 0, bw, bh, SRCCOPY);
        SelectObject(mem, old);
        DeleteObject(bmp);
        DeleteDC(mem);
    }
}

fn draw(g: &Gdi, l: &Layout, w: &Win) {
    let s = l.scale;
    let u = |v: f64| ((v * s).round() as i32).max(1);
    // The grey theme (portrait, bigger controls) uses larger type.
    let f = Fonts::new(if w.spec.theme == Theme::Grey { s * 1.3 } else { s });
    let sh = &w.shared;
    let spec = w.spec;
    let values = sh.snapshot();
    let bypassed = sh.get(spec.bypass_id) >= 0.5;
    PAL.with(|c| c.set(if spec.theme == Theme::Grey { GREY } else { WALNUT }));
    let grey = spec.theme == Theme::Grey;
    let full = Rect::new(0, 0, l.width, l.height);
    g.fill(&full, p().panel);

    if !grey {
    // Walnut veneer: under the preset bar, down both sides and along the bottom.
    let (wt, ws, wb) = (u(6.0), u(8.0), u(14.0));
    g.wood(&Rect::new(0, l.bar.bottom, l.width, l.bar.bottom + wt), false, 11);
    g.wood(&Rect::new(0, l.bar.bottom + wt, ws, l.height - wb), true, 23);
    g.wood(&Rect::new(l.width - ws, l.bar.bottom + wt, l.width, l.height - wb), true, 37);
    g.wood(&Rect::new(0, l.height - wb, l.width, l.height), false, 51);
    g.fill(&Rect::new(ws, l.bar.bottom + wt, l.width - ws, l.bar.bottom + wt + u(1.0)), p().edge);

    // Preset bar.
    g.fill(&l.bar, p().bar);
    g.fill(&Rect::new(0, l.bar.bottom - u(1.0), l.width, l.bar.bottom), p().edge);
    for (r, right) in [(l.prev, false), (l.next, true)] {
        g.boxed(&r, p().btn, p().btn_edge, u(1.0));
        let (cx, cy) = r.center();
        let a = u(4.0);
        let pts = if right {
            [(cx - a / 2, cy - a), (cx - a / 2, cy + a), (cx + a, cy)]
        } else {
            [(cx + a / 2, cy - a), (cx + a / 2, cy + a), (cx - a, cy)]
        };
        g.polygon(&pts, p().text);
    }
    g.boxed(&l.name, p().field, p().edge, u(1.0));
    let name = match matching_preset(spec, &values, w.user.as_ref()) {
        Some(i) => spec.presets.get(i).map(|p| p.name.to_string()).unwrap_or_else(|| "User".to_string()),
        None => format!("{} *", spec.presets.get(w.last_preset).map(|p| p.name).unwrap_or("Default")),
    };
    let name_r = Rect::new(l.name.left + u(8.0), l.name.top, l.name.right - u(20.0), l.name.bottom);
    g.text(&name_r, &name, f.name, p().rust, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
    let (ax, ay) = (l.name.right - u(11.0), l.name.center().1);
    g.polygon(&[(ax - u(3.5), ay - u(2.0)), (ax + u(3.5), ay - u(2.0)), (ax, ay + u(2.5))], p().dim);
    // Save: an arrow into a tray.
    g.boxed(&l.save, p().btn, p().btn_edge, u(1.0));
    let (cx, cy) = l.save.center();
    g.polyline(&[(cx - u(5.0), cy + u(1.0)), (cx - u(5.0), cy + u(5.0)), (cx + u(5.0), cy + u(5.0)), (cx + u(5.0), cy + u(1.0))], p().text, u(1.4));
    g.polyline(&[(cx, cy - u(6.0)), (cx, cy + u(1.0))], p().text, u(1.4));
    g.polygon(&[(cx - u(3.5), cy - u(1.0)), (cx + u(3.5), cy - u(1.0)), (cx, cy + u(3.0))], p().text);
    // Reset: a circular arrow.
    g.boxed(&l.reset, p().btn, p().btn_edge, u(1.0));
    let (cx, cy) = l.reset.center();
    let ring: Vec<(i32, i32)> = (0..=24).map(|i| layout::polar(cx, cy, 5.0 * s, 100.0 + 290.0 * f64::from(i) / 24.0)).collect();
    g.polyline(&ring, p().text, u(1.4));
    let tip = layout::polar(cx, cy, 5.0 * s, 100.0);
    g.polygon(&[(tip.0 - u(3.5), tip.1 - u(3.0)), (tip.0 + u(1.5), tip.1 - u(3.5)), (tip.0 - u(0.5), tip.1 + u(1.5))], p().text);
    // The bundle name, right-aligned before Bypass.
    // SAFETY: letter spacing on our memory DC, reset right after.
    unsafe { SetTextCharacterExtra(g.hdc, u(2.0)) };
    g.text(&l.bundle, &BUNDLE_NAME.to_uppercase(), f.small, p().dim, DT_RIGHT | DT_VCENTER | DT_SINGLELINE);
    // SAFETY: as above.
    unsafe { SetTextCharacterExtra(g.hdc, 0) };
    // Bypass.
    let (bf, bt) = if bypassed { (p().rust, p().field) } else { (p().btn, p().text) };
    g.boxed(&l.bypass, bf, p().btn_edge, u(1.0));
    g.text(&l.bypass, "BYPASS", f.small, bt, CENTER);

    // Title row: the accent line through the wordmark.
    for (a, b) in [l.line_left, l.line_right] {
        g.fill(&Rect::new(a, l.line_y - u(1.0), b, l.line_y + u(1.0)), p().rust);
    }
    // SAFETY: letter spacing on our memory DC, reset right after.
    unsafe { SetTextCharacterExtra(g.hdc, u(3.0)) };
    g.text(&l.wordmark, &spec.title.to_uppercase(), f.word, p().text, CENTER);
    // SAFETY: as above.
    unsafe { SetTextCharacterExtra(g.hdc, 0) };

    }

    // Knobs.
    for k in &l.knobs {
        draw_knob(g, &f, k, values.get(k.id as usize).copied().unwrap_or(0.0), s, bypassed, w.hover == Some(Hit::Knob(k.id)), spec);
    }

    // Selector groups.
    if let (Some(sel), Some(ss)) = (&l.selector, spec.selector) {
        draw_selector(g, &f, sel, &ss, spec.model.to_plain(ss.id, sh.get(ss.id)).round().max(0.0) as usize, s);
    }

    // Dropdowns.
    for (i, (d, ds)) in l.dropdowns.iter().zip(spec.dropdowns.iter()).enumerate() {
        let cur = spec.model.to_plain(ds.id, sh.get(ds.id)).round().max(0.0) as usize;
        let hot = w.hover == Some(Hit::Dropdown(i));
        g.boxed(&d.rect, p().btn, if hot { p().rust } else { p().btn_edge }, u(1.0));
        g.text(&d.caption, ds.caption, f.small, p().dim, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        g.text(&d.value, ds.options.get(cur).copied().unwrap_or(""), f.label, p().text, CENTER);
        let (ax, ay) = d.arrow.center();
        g.polyline(&[(ax - u(4.0), ay - u(2.0)), (ax, ay + u(2.0)), (ax + u(4.0), ay - u(2.0))], p().text, u(1.4));
    }

    // Number boxes (typed values).
    for (i, (nb, ns)) in l.numbers.iter().zip(spec.numbers.iter()).enumerate() {
        let typing = w.typing.as_ref().filter(|t| t.index == i);
        let hot = typing.is_some() || w.hover == Some(Hit::Number(i));
        g.boxed(&nb.rect, p().btn, if hot { p().rust } else { p().btn_edge }, u(1.0));
        g.text(&nb.caption, ns.caption, f.small, p().dim, CENTER);
        let shown = match typing {
            Some(t) => format!("{}{}", t.text, if t.fresh { "" } else { "|" }),
            None => format!("{:.1}", spec.model.to_plain(ns.id, sh.get(ns.id))),
        };
        g.text(&nb.value, &shown, f.label, p().text, CENTER);
    }

    // Dividers.
    for r in &l.rules {
        g.fill(&Rect::new(r.left, r.top, r.right, r.top + u(1.0)), p().btn_edge);
    }

    // Fixed texts.
    for (r, t) in &l.labels {
        g.text(r, t, f.label, p().dim, CENTER);
    }

    // Radio buttons.
    for (q, qs) in l.radios.iter().zip(spec.radios.iter()) {
        let on = (spec.model.to_plain(qs.id, sh.get(qs.id)) - qs.value).abs() < 0.5;
        let bare = qs.label.is_empty();
        let rad = f64::from(qs.r);
        g.ellipse(q.dot.0, q.dot.1, u(rad), p().btn, if on { p().rust } else { p().btn_edge }, u(2.0));
        if on {
            g.ellipse(q.dot.0, q.dot.1, u(rad * 0.5), p().rust, p().rust, u(1.0));
        }
        if !bare {
            g.text(&q.label, qs.label, f.label, if on { p().text } else { p().dim }, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        }
    }

    // Switches.
    for (t, ts) in l.toggles.iter().zip(spec.toggles.iter()) {
        let on = sh.get(ts.id) >= 0.5;
        if ts.glyph == Glyph::Check {
            g.boxed(&t.rect, p().btn, if on { p().rust } else { p().btn_edge }, u(2.0));
            if on {
                let (cx, cy) = t.rect.center();
                g.polyline(&[(cx - u(8.0), cy), (cx - u(2.5), cy + u(6.0)), (cx + u(9.0), cy - u(8.0))], p().rust, u(3.0));
            }
            g.text(&t.label, ts.label, f.label, if on { p().text } else { p().dim }, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
            continue;
        }
        g.text(&t.label, ts.label, f.small, p().dim, CENTER);
        g.boxed(&t.rect, if on && ts.glyph == Glyph::Phase { p().rust_dim } else { p().btn }, if on { p().rust } else { p().btn_edge }, u(1.0));
        let (cx, cy) = t.rect.center();
        match ts.glyph {
            Glyph::Phase => {
                let pr = u(5.0);
                let fill = if on { p().rust_dim } else { p().btn };
                let ink = if on { p().pointer } else { p().dim };
                g.ellipse(cx, cy, pr, fill, ink, u(1.3));
                g.polyline(&[(cx + pr, cy - pr - u(1.0)), (cx - pr, cy + pr + u(1.0))], ink, u(1.3));
            }
            Glyph::Lamp | Glyph::Check => {
                let a = u(4.0);
                g.fill(&Rect::new(cx - a, cy - a, cx + a, cy + a), if on { p().rust } else { p().edge });
            }
        }
        g.text(&t.value, if on { ts.on_text } else { ts.off_text }, f.small, if on { p().rust } else { p().dim }, CENTER);
    }

    // Input meter: peak bar, with a marker where the signal starts to hit the shaper.
    if let (Some(m), Some(ms)) = (&l.meter, spec.meter) {
        g.text(&m.caption, ms.caption, f.small, p().dim, DT_RIGHT | DT_VCENTER | DT_SINGLELINE);
        g.boxed(&m.bar, p().field, p().edge, u(1.0));
        let inner = Rect::new(m.bar.left + u(2.0), m.bar.top + u(2.0), m.bar.right - u(2.0), m.bar.bottom - u(2.0));
        let peak_db = paint::level_db(sh.meter());
        let at = |db: f32| inner.left + (paint::meter_fraction(db, ms.floor_db, ms.ceil_db) * inner.width() as f32).round() as i32;
        let marker = spec.model.meter_marker_db(&values).filter(|db| *db >= ms.floor_db && *db <= ms.ceil_db);
        let (seg, gap) = (u(4.0), u(1.0));
        let lit = at(peak_db);
        let mut x = inner.left;
        while x + seg <= inner.right {
            let hot = marker.is_some_and(|db| x + seg / 2 >= at(db));
            let color = if x + seg <= lit { if hot { p().pointer } else { p().rust } } else { p().track };
            g.fill(&Rect::new(x, inner.top, x + seg, inner.bottom), color);
            x += seg + gap;
        }
        if let Some(db) = marker {
            let mx = at(db);
            g.fill(&Rect::new(mx - u(1.0), m.bar.top - u(2.0), mx + u(1.0), m.bar.bottom + u(2.0)), p().text);
        }
        let readout = if peak_db > -90.0 { format!("{peak_db:.0} dB") } else { "-inf".to_string() };
        g.text(&m.readout, &readout, f.small, p().dim, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
    }

    // Resize grip.
    for i in 1..=3 {
        let o = u(4.0) * i;
        g.polyline(&[(l.width - o - u(2.0), l.height - u(2.0)), (l.width - u(2.0), l.height - o - u(2.0))], p().dim, u(1.0));
    }

    // Tooltip.
    if w.tip
        && let Some(h) = w.hover
    {
        let text = layout::tooltip(spec, h);
        if !text.is_empty() {
            let pad = u(6.0);
            let (tw, th) = g.measure(text, f.small, u(260.0));
            let (hx, hy) = (w.hover_at.0 * SS + u(12.0), w.hover_at.1 * SS + u(18.0));
            let bw = tw + 2 * pad;
            let bh = th + 2 * pad;
            let x = hx.min(l.width - bw - u(4.0)).max(u(4.0));
            let y = if hy + bh > l.height - u(4.0) { (w.hover_at.1 * SS - bh - u(8.0)).max(u(4.0)) } else { hy };
            let r = Rect::new(x, y, x + bw, y + bh);
            g.boxed(&r, p().tip_bg, p().rust_dim, u(1.0));
            g.text(&Rect::new(x + pad, y + pad, x + pad + tw, y + pad + th), text, f.small, p().text, DT_LEFT | DT_WORDBREAK);
        }
    }
}

/// One row of exclusive buttons with its marks, caption rule and the selected name.
fn draw_selector(g: &Gdi, f: &Fonts, sel: &layout::Selector, ss: &SelectorSpec, cur: usize, s: f64) {
    let u = |v: f64| ((v * s).round() as i32).max(1);
    for (i, (r, mark)) in sel.buttons.iter().zip(sel.marks.iter()).enumerate() {
        let on = i == cur;
        g.text(mark, ss.options.get(i).map_or("", |o| o.mark), f.label, if on { p().text } else { p().dim }, CENTER);
        g.boxed(r, p().btn, if on { p().rust } else { p().btn_edge }, u(1.0));
        // The lamp: a small square lit in the accent when active.
        let (cx, cy) = r.center();
        let a = u(4.0);
        g.fill(&Rect::new(cx - a, cy - a, cx + a, cy + a), if on { p().rust } else { p().edge });
    }
    let mc = sel.caption;
    let (_, my) = mc.center();
    if let (Some(first), Some(last)) = (sel.buttons.first(), sel.buttons.last()) {
        g.fill(&Rect::new(first.left, my, mc.left + u(22.0), my + u(1.0)), p().dim);
        g.fill(&Rect::new(mc.right - u(22.0), my, last.right, my + u(1.0)), p().dim);
    }
    g.text(&mc, ss.caption, f.small, p().dim, CENTER);
    let cur_name = ss.options.get(cur).map_or("", |o| o.name);
    g.text(&sel.name, &cur_name.to_ascii_uppercase(), f.label, p().rust, CENTER);
}

fn draw_knob(g: &Gdi, f: &Fonts, k: &Knob, n: f64, s: f64, bypassed: bool, hot: bool, spec: &UiSpec) {
    let u = |v: f64| ((v * s).round() as i32).max(1);
    let ring = f64::from(k.r) + 6.0 * s;
    let thick = if k.big { u(3.0) } else { u(2.0) };
    g.polyline(&layout::arc_points(k.cx, k.cy, ring, 0.0, 1.0), p().track, thick);
    // A bipolar knob's arc starts at its origin.
    let from = k.origin;
    if (n - from).abs() > 1e-4 {
        g.polyline(&layout::arc_points(k.cx, k.cy, ring, from.min(n), from.max(n)), if bypassed { p().rust_dim } else { p().rust }, thick);
    }
    g.ellipse(k.cx, k.cy, k.r, p().dial, p().outline, u(2.0));
    // A faint inner ring for depth, then the pointer.
    g.ellipse(k.cx, k.cy, k.r - u(4.0), p().dial, p().inner_ring, u(1.0));
    let a = layout::knob_angle(n);
    let r = f64::from(k.r);
    g.polyline(
        &[layout::polar(k.cx, k.cy, r * 0.3, a), layout::polar(k.cx, k.cy, r * 0.88, a)],
        p().pointer2,
        if k.big { u(1.6) } else { u(1.3) },
    );
    let value = spec.model.format(k.id, n);
    let vcol = if hot { p().text } else { p().dim };
    if k.below {
        g.text(&k.label, k.caption, f.label, p().text, CENTER);
        g.text(&k.value, &spec.model.format_short(k.id, n), f.small, vcol, CENTER);
    } else if k.big {
        g.text(&k.label, k.caption, f.label, p().text, CENTER);
        g.text(&k.end_left, k.ends.0, f.small, p().dim, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        g.text(&k.end_right, k.ends.1, f.small, p().dim, DT_RIGHT | DT_VCENTER | DT_SINGLELINE);
        g.text(&k.value, &value, f.small, vcol, CENTER);
    } else {
        g.text(&k.label, k.caption, f.label, p().text, DT_RIGHT | DT_VCENTER | DT_SINGLELINE);
        g.text(&k.value, &value, f.small, vcol, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        g.text(&k.end_left, k.ends.0, f.small, p().dim, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        g.text(&k.end_right, k.ends.1, f.small, p().dim, DT_RIGHT | DT_VCENTER | DT_SINGLELINE);
    }
}

/// Renders the editor for `shared` into an off-screen parent window, captures it with
/// PrintWindow and saves a PNG to `path`, with `hover` showing its tooltip. Also resizes through
/// `onSize` and detaches. For tests and screenshots (feature `capture`); never shown on a monitor.
#[cfg(feature = "capture")]
pub fn capture_png(shared: Arc<Shared>, spec: &'static UiSpec, hover: Option<Hit>, path: &std::path::Path) -> Result<(), String> {
    use windows_sys::Win32::Storage::Xps::PrintWindow;
    let view = create_view(shared, spec);
    if view.is_null() {
        return Err("no view".into());
    }
    // SAFETY: Win32 and COM calls on objects created here; everything is released before return.
    unsafe {
        let v = ComRef::from_raw(view).ok_or("view")?;
        if v.isPlatformTypeSupported(c"HWND".as_ptr()) != kResultTrue {
            return Err("HWND unsupported".into());
        }
        let mut r = ViewRect { left: 0, top: 0, right: 0, bottom: 0 };
        v.getSize(&mut r);
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        if (w, h) != layout::default_size(spec) {
            return Err(format!("default size {w}x{h}"));
        }
        let class = wide("STATIC");
        let parent = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class.as_ptr(),
            std::ptr::null(),
            WS_POPUP,
            -20000,
            -20000,
            w,
            h,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        );
        if parent.is_null() {
            return Err("parent window".into());
        }
        if v.attached(parent, c"HWND".as_ptr()) != kResultOk {
            return Err("attached".into());
        }
        // Shown far off-screen (never on a monitor), so the window has real content to print.
        ShowWindow(parent, SW_SHOWNOACTIVATE);
        let child = GetWindow(parent, GW_CHILD);
        if child.is_null() {
            return Err("no child".into());
        }
        if let Some(win) = (GetWindowLongPtrW(child, GWLP_USERDATA) as *mut Win).as_mut() {
            let l = layout::layout(spec, w, h);
            if let Some(hv) = hover {
                let at = match hv {
                    Hit::Option(i) => l.selector.as_ref().and_then(|s| s.buttons.get(i)).map(Rect::center),
                    Hit::Toggle(i) => l.toggles.get(i).map(|t| t.rect.center()),
                    Hit::Knob(id) => l.knobs.iter().find(|k| k.id == id).map(|k| (k.cx, k.cy)),
                    Hit::Meter => l.meter.as_ref().map(|m| m.bar.center()),
                    Hit::Bypass => Some(l.bypass.center()),
                    _ => None,
                };
                win.hover = Some(hv);
                win.hover_at = at.unwrap_or((w / 2, h / 2));
                win.tip = true;
            }
        }
        let screen = GetDC(std::ptr::null_mut());
        let mem = CreateCompatibleDC(screen);
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, bmp);
        UpdateWindow(child);
        let pw = PrintWindow(child, mem, 0);
        SelectObject(mem, old);
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = w;
        info.bmiHeader.biHeight = -h;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        let mut px = vec![0u8; (w * h * 4) as usize];
        let lines = GetDIBits(mem, bmp, 0, h as u32, px.as_mut_ptr().cast(), &mut info, DIB_RGB_COLORS);
        DeleteObject(bmp);
        DeleteDC(mem);
        ReleaseDC(std::ptr::null_mut(), screen);
        if pw == 0 || lines != h {
            return Err("PrintWindow / GetDIBits failed".into());
        }
        // BGRA -> RGBA, opaque.
        for p in px.as_chunks_mut::<4>().0 {
            p.swap(0, 2);
            p[3] = 255;
        }
        let distinct: std::collections::HashSet<[u8; 3]> = px.as_chunks::<4>().0.iter().map(|p| [p[0], p[1], p[2]]).collect();
        if distinct.len() <= 50 {
            return Err(format!("the capture is blank: {} colours", distinct.len()));
        }
        image::save_buffer(path, &px, w as u32, h as u32, image::ColorType::Rgba8).map_err(|e| format!("save {path:?}: {e}"))?;
        // Resize through onSize and detach.
        let (bw, bh) = (w * 3 / 2, h * 3 / 2);
        let mut big = ViewRect { left: 0, top: 0, right: bw, bottom: bh };
        v.checkSizeConstraint(&mut big);
        v.onSize(&mut big);
        let mut rc = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        GetClientRect(child, &mut rc);
        let resized = (rc.right, rc.bottom) == (big.right, big.bottom);
        let removed = v.removed() == kResultOk && GetWindow(parent, GW_CHILD).is_null();
        DestroyWindow(parent);
        drop(vst3::ComPtr::from_raw(view));
        if !resized {
            return Err(format!("resize gave {}x{}", rc.right, rc.bottom));
        }
        if !removed {
            return Err("removed did not destroy the child".into());
        }
    }
    Ok(())
}

/// Applies (or drops) the number being typed: the text is the parameter's plain value.
fn finish_typing(w: &mut Win, apply: bool) {
    let Some(t) = w.typing.take() else { return };
    if !apply {
        return;
    }
    let (Some(n), Ok(v)) = (w.spec.numbers.get(t.index), t.text.trim().parse::<f64>()) else { return };
    if v.is_finite() {
        edit(&w.shared, n.id, w.spec.model.to_normalized(n.id, v));
    }
}

/// A character typed into the number box: digits, a point, a minus; Backspace, Enter, Esc.
fn type_char(hwnd: HWND, w: &mut Win, c: u32) {
    let Some(t) = w.typing.as_mut() else { return };
    match c {
        13 => finish_typing(w, true),
        27 => finish_typing(w, false),
        8 => {
            if t.fresh {
                t.text.clear();
                t.fresh = false;
            } else {
                t.text.pop();
            }
        }
        _ => {
            let ch = match char::from_u32(c) {
                Some(',') => '.',
                Some(ch) if ch.is_ascii_digit() || ch == '.' || ch == '-' => ch,
                _ => return,
            };
            if t.fresh {
                t.text.clear();
                t.fresh = false;
            }
            if t.text.len() < 8 {
                t.text.push(ch);
            }
        }
    }
    invalidate(hwnd);
}
