//! Update check, Windows part: the WinHTTP fetch of the latest release, the background check
//! thread and the prompt. Nothing here runs on the audio thread. The editor calls `spawn_check`
//! when its window opens; the thread never touches the window except to `PostMessageW` one
//! message, and the prompt itself is shown by the window procedure on the UI thread
//! (`on_update_ready`).

use crate::update::{self, Choice, LATEST_URL, Release};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Networking::WinHttp::*;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    IDCANCEL, IDYES, IsWindow, MB_ICONINFORMATION, MB_YESNOCANCEL, MessageBoxW, PostMessageW, SW_SHOWNORMAL, WM_APP,
};

/// Posted to the editor window when a release to prompt about is waiting in its slot.
pub const WM_UPDATE_READY: u32 = WM_APP + 0x42;

/// Where the check thread leaves its result for the UI thread.
pub type Slot = Arc<Mutex<Option<Release>>>;

/// At most one prompt per process, however many editors are open.
static PROMPTED: AtomicBool = AtomicBool::new(false);
/// At most one network attempt per process.
static ATTEMPTED: AtomicBool = AtomicBool::new(false);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Closes a WinHTTP handle on drop.
struct Handle(*mut c_void);

impl Handle {
    fn new(h: *mut c_void) -> Option<Handle> {
        (!h.is_null()).then_some(Handle(h))
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: a handle returned by WinHTTP, closed once.
        unsafe { WinHttpCloseHandle(self.0) };
    }
}

/// One GET of the latest-release JSON, with ~5 s timeouts. Any failure is `None`.
pub fn fetch_latest_json() -> Option<String> {
    let agent = wide(&format!("BentSignal-Plugin/{}", update::running_version()));
    let host = wide("api.github.com");
    let verb = wide("GET");
    let path = wide(LATEST_URL.strip_prefix("https://api.github.com")?);
    let headers = wide(&format!("User-Agent: BentSignal-Plugin/{}\r\nAccept: application/vnd.github+json\r\n", update::running_version()));
    // SAFETY: plain WinHTTP calls on handles owned by `Handle` guards; every buffer outlives its call.
    unsafe {
        let session = Handle::new(WinHttpOpen(agent.as_ptr(), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, std::ptr::null(), std::ptr::null(), 0))
            .or_else(|| Handle::new(WinHttpOpen(agent.as_ptr(), WINHTTP_ACCESS_TYPE_DEFAULT_PROXY, std::ptr::null(), std::ptr::null(), 0)))?;
        if WinHttpSetTimeouts(session.0, 5000, 5000, 5000, 5000) == 0 {
            return None;
        }
        let conn = Handle::new(WinHttpConnect(session.0, host.as_ptr(), 443, 0))?;
        let req = Handle::new(WinHttpOpenRequest(
            conn.0,
            verb.as_ptr(),
            path.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            WINHTTP_FLAG_SECURE,
        ))?;
        if WinHttpSendRequest(req.0, headers.as_ptr(), u32::MAX, std::ptr::null(), 0, 0, 0) == 0 || WinHttpReceiveResponse(req.0, std::ptr::null_mut()) == 0 {
            return None;
        }
        let mut status: u32 = 0;
        let mut len = std::mem::size_of::<u32>() as u32;
        if WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            std::ptr::null(),
            (&mut status as *mut u32).cast(),
            &mut len,
            std::ptr::null_mut(),
        ) == 0
            || status != 200
        {
            return None;
        }
        let mut body = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            let mut got: u32 = 0;
            if WinHttpReadData(req.0, buf.as_mut_ptr().cast(), buf.len() as u32, &mut got) == 0 {
                return None;
            }
            if got == 0 {
                break;
            }
            body.extend_from_slice(&buf[..got as usize]);
            if body.len() > 1 << 20 {
                return None;
            }
        }
        String::from_utf8(body).ok()
    }
}

/// The latest published release, from the network.
pub fn fetch_latest() -> Option<Release> {
    update::parse_release(&fetch_latest_json()?)
}

/// Starts the background check for an editor window. Never blocks; a no-op on any failure.
pub fn spawn_check(h: usize, slot: Slot) {
    let _ = std::thread::Builder::new().name("bentsignal-update".into()).spawn(move || {
        if PROMPTED.load(Ordering::Relaxed) || update::disabled() {
            return;
        }
        let Some(dir) = update::state_dir() else { return };
        let found = update::check(&dir, &update::running_version(), update::now_secs(), update::forced(), || if ATTEMPTED.swap(true, Ordering::AcqRel) { None } else { fetch_latest() });
        if let Some(r) = found {
            if let Ok(mut s) = slot.lock() {
                *s = Some(r);
            }
            // SAFETY: PostMessageW is thread-safe; a destroyed window just makes it fail.
            unsafe { PostMessageW(h as HWND, WM_UPDATE_READY, 0, 0) };
        }
    });
}

/// Shows the prompt (UI thread, from the window procedure). Must not touch the editor's state
/// afterwards: the host may destroy the window while the box is up.
pub fn on_update_ready(h: usize, slot: &Slot, plugin: &str) {
    let hwnd = h as HWND;
    let release = slot.lock().ok().and_then(|mut s| s.take());
    let Some(release) = release else { return };
    // SAFETY: a plain window-handle test.
    if unsafe { IsWindow(hwnd) } == 0 || PROMPTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let current = update::running_version();
    let text = wide(&update::message(&release, &current));
    let title = wide(&format!("{plugin} update"));
    // SAFETY: modal box owned by our live window; the strings outlive the call.
    let answer = unsafe { MessageBoxW(hwnd, text.as_ptr(), title.as_ptr(), MB_YESNOCANCEL | MB_ICONINFORMATION) };
    let choice = match answer {
        IDYES => Choice::Open,
        IDCANCEL => Choice::Skip,
        _ => Choice::Later,
    };
    if let Some(dir) = update::state_dir() {
        update::record(&dir, choice, &release.tag, update::now_secs());
    }
    if choice == Choice::Open {
        let url = wide(&update::safe_url(&release.url));
        let open = wide("open");
        // SAFETY: opens the default browser; no owner window (it may be gone by now).
        unsafe { ShellExecuteW(std::ptr::null_mut(), open.as_ptr(), url.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL) };
    }
}
