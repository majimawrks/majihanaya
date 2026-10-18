//! The few Win32 calls the app needs. Kept here so the data code stays portable.

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, IsWindowVisible, PostMessageW, SW_HIDE, SW_RESTORE, SW_SHOW, SetForegroundWindow,
    ShowWindow, WM_CLOSE,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, w};
use std::path::Path;

/// Opens a file with the user's default handler for its type.
pub fn open_file(path: &Path) {
    // SAFETY: Win32 call with owned, NUL-terminated strings that outlive it.
    unsafe {
        ShellExecuteW(None, w!("open"), &HSTRING::from(path.as_os_str()), None, None, SW_SHOWNORMAL);
    }
}

/// Distinct from the bare project name so `FindWindowW` never matches an
/// Explorer window open on a folder called "majihanaya".
pub const WINDOW_TITLE: &str = "majihanaya - monitor";

/// Holds the named mutex for the life of the process.
pub struct SingleInstance(#[allow(dead_code)] HANDLE);

/// `None` when another instance already owns the mutex.
pub fn single_instance() -> Option<SingleInstance> {
    // SAFETY: plain Win32 call with a static name; the handle is kept until exit.
    unsafe {
        let handle = CreateMutexW(None, false, w!("Local\\majihanaya")).ok()?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return None;
        }
        Some(SingleInstance(handle))
    }
}

/// Brings the running instance's window to the front.
pub fn activate_existing() {
    // SAFETY: FindWindowW/ShowWindow on a window we only look up by title.
    unsafe {
        if let Ok(hwnd) = FindWindowW(None, &HSTRING::from(WINDOW_TITLE)) {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

/// Our own top-level window, as a plain integer so it can cross into callbacks.
#[derive(Clone, Copy)]
pub struct Window(isize);

impl Window {
    pub fn from_raw(hwnd: isize) -> Self {
        Self(hwnd)
    }

    fn hwnd(self) -> HWND {
        HWND(self.0 as *mut _)
    }

    pub fn is_visible(self) -> bool {
        // SAFETY: read-only query on our own window handle.
        unsafe { IsWindowVisible(self.hwnd()).as_bool() }
    }

    pub fn show(self) {
        // SAFETY: our own window handle, owned by this process for its lifetime.
        unsafe {
            let _ = ShowWindow(self.hwnd(), SW_SHOW);
            let _ = SetForegroundWindow(self.hwnd());
        }
    }

    pub fn hide(self) {
        // SAFETY: as above.
        unsafe {
            let _ = ShowWindow(self.hwnd(), SW_HIDE);
        }
    }

    pub fn toggle(self) {
        if self.is_visible() { self.hide() } else { self.show() }
    }

    /// Asks the event loop to close, even while the window is hidden.
    pub fn post_close(self) {
        // SAFETY: as above.
        unsafe {
            let _ = PostMessageW(Some(self.hwnd()), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}
