//! Edits to the host through its `IComponentHandler` (UI thread), plus the string helpers the
//! VST3 info structs need.

use crate::shared::Shared;
use std::ffi::c_char;
use std::sync::atomic::Ordering;
use vst3::Steinberg::Vst::*;
use vst3::{ComPtr, ComRef};

pub fn copy_c8(src: &str, dst: &mut [c_char]) {
    let n = dst.len().saturating_sub(1);
    let mut i = 0;
    for (d, s) in dst.iter_mut().zip(src.bytes()).take(n) {
        *d = s as c_char;
        i += 1;
    }
    if let Some(z) = dst.get_mut(i) {
        *z = 0;
    }
}

pub fn copy_c16(src: &str, dst: &mut [TChar]) {
    let n = dst.len().saturating_sub(1);
    let mut i = 0;
    for (d, s) in dst.iter_mut().zip(src.encode_utf16()).take(n) {
        *d = s as TChar;
        i += 1;
    }
    if let Some(z) = dst.get_mut(i) {
        *z = 0;
    }
}

fn with_handler(shared: &Shared, f: impl FnOnce(ComRef<'_, IComponentHandler>)) {
    let p = shared.handler.load(Ordering::Acquire) as *mut IComponentHandler;
    // SAFETY: a non-zero address is an AddRef'd handler from `set_handler`, released only on the
    // UI thread (terminate / replace / drop), the thread edits come from.
    if let Some(h) = unsafe { ComRef::from_raw(p) } {
        f(h);
    }
}

pub fn begin_edit(shared: &Shared, id: u32) {
    // SAFETY: a valid handler (see `with_handler`).
    with_handler(shared, |h| unsafe {
        h.beginEdit(id);
    });
}

/// Sets the value locally (the editor follows at once) and tells the host.
pub fn perform_edit(shared: &Shared, id: u32, n: f64) {
    shared.set(id, n);
    let n = shared.get(id);
    // SAFETY: a valid handler (see `with_handler`).
    with_handler(shared, |h| unsafe {
        h.performEdit(id, n);
    });
    shared.notify_edit(id, n);
}

/// Tells the host that `id` of this instance is now `n` (the value is already stored): a whole
/// begin/perform/end edit, without the edit hook. For changes that come from another instance.
pub fn notify_host(shared: &Shared, id: u32, n: f64) {
    // SAFETY: a valid handler (see `with_handler`).
    with_handler(shared, |h| unsafe {
        h.beginEdit(id);
        h.performEdit(id, n);
        h.endEdit(id);
    });
}

pub fn end_edit(shared: &Shared, id: u32) {
    // SAFETY: a valid handler (see `with_handler`).
    with_handler(shared, |h| unsafe {
        h.endEdit(id);
    });
}

/// A complete one-shot edit (a click on a button or a menu choice).
pub fn edit(shared: &Shared, id: u32, n: f64) {
    begin_edit(shared, id);
    perform_edit(shared, id, n);
    end_edit(shared, id);
}

/// Sends pending `restartComponent` flags (see `Shared::restart`) to the host. UI thread only.
pub fn flush_restart(shared: &Shared) {
    if shared.restart.load(Ordering::Acquire) == 0 {
        return;
    }
    with_handler(shared, |h| {
        let flags = shared.restart.swap(0, Ordering::AcqRel);
        if flags != 0 {
            // SAFETY: a valid handler (see `with_handler`).
            unsafe { h.restartComponent(flags) };
        }
    });
}

/// Keeps (AddRef) the host's handler, releasing the previous one; null clears it.
///
/// # Safety
/// `handler` is null or a valid `IComponentHandler`, and this runs on the UI thread.
pub unsafe fn set_handler(shared: &Shared, handler: *mut IComponentHandler) {
    // SAFETY: per the contract; we keep our own reference.
    let new = unsafe { ComRef::from_raw(handler) }.map(|h| h.to_com_ptr().into_raw() as usize).unwrap_or(0);
    let old = shared.handler.swap(new, Ordering::AcqRel) as *mut IComponentHandler;
    // SAFETY: `old` was AddRef'd by us (or is null); this releases that reference.
    drop(unsafe { ComPtr::from_raw(old) });
}
