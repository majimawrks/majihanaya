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

/// Release builds have no console; reuse the parent's (the shell that ran
/// `--dump`) so stdout reaches it. Harmless when there is none.
pub fn attach_parent_console() {
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // SAFETY: plain Win32 call; failure just means no console to attach.
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

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

    /// Moves the window so its *visible* frame (title bar included, invisible
    /// resize borders excluded) sits in the tray corner of the work area.
    pub fn snap_to_tray_corner(self) {
        use windows::Win32::Foundation::RECT;
        use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
        use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos};
        let Some(((l, t, r, b), corner, scale)) = tray_corner() else { return };
        let gap = (12.0 * scale).round() as i32;
        // SAFETY: queries and moves our own window; RECTs are plain out-params.
        unsafe {
            let (mut outer, mut frame) = (RECT::default(), RECT::default());
            if GetWindowRect(self.hwnd(), &mut outer).is_err()
                || DwmGetWindowAttribute(
                    self.hwnd(),
                    DWMWA_EXTENDED_FRAME_BOUNDS,
                    &mut frame as *mut _ as *mut _,
                    std::mem::size_of::<RECT>() as u32,
                )
                .is_err()
            {
                frame = outer;
            }
            let (fw, fh) = (frame.right - frame.left, frame.bottom - frame.top);
            let fx = match corner {
                Corner::BottomLeft => l + gap,
                _ => r - gap - fw,
            };
            let fy = match corner {
                Corner::TopRight => t + gap,
                _ => b - gap - fh,
            };
            // SetWindowPos takes the outer rect: shift by the invisible border.
            let (x, y) = (fx - (frame.left - outer.left), fy - (frame.top - outer.top));
            let _ = SetWindowPos(self.hwnd(), None, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }

    /// Asks the event loop to close, even while the window is hidden.
    pub fn post_close(self) {
        // SAFETY: as above.
        unsafe {
            let _ = PostMessageW(Some(self.hwnd()), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

/// Tray icon size for the current DPI: the 16 px drawing at 100–124%,
/// the 32 px one above (the shell scales it to 20/24/32).
pub fn tray_icon_size() -> u32 {
    use windows::Win32::UI::HiDpi::GetDpiForSystem;
    // SAFETY: plain query, no arguments.
    let dpi = unsafe { GetDpiForSystem() };
    if dpi >= 120 { 32 } else { 16 }
}

const RUN_KEY: windows::core::PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const RUN_VALUE: windows::core::PCWSTR = w!("majihanaya");

/// Whether our HKCU Run entry exists (Task Manager's Startup tab can still
/// disable it; that state isn't read here).
pub fn autostart_enabled() -> bool {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};
    // SAFETY: size-only query of a value we own; no buffer is passed.
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ, None, None, None).is_ok() }
}

/// Adds or removes the HKCU Run entry pointing at the running exe.
pub fn set_autostart(on: bool) -> windows::core::Result<()> {
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey, RegCreateKeyExW,
        RegDeleteValueW, RegSetValueExW,
    };
    // SAFETY: we open, write and close one key under HKCU; the data buffer
    // is a NUL-terminated UTF-16 string that outlives the call.
    unsafe {
        let mut key = HKEY::default();
        RegCreateKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, None, REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, None, &mut key, None)
            .ok()?;
        let result = if on {
            let exe = std::env::current_exe().map_err(|_| windows::core::Error::from_thread())?;
            // Quoted: the path may contain spaces.
            let data: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain([0]).collect();
            let bytes = std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), data.len() * 2);
            RegSetValueExW(key, RUN_VALUE, None, REG_SZ, Some(bytes)).ok()
        } else {
            match RegDeleteValueW(key, RUN_VALUE).ok() {
                Err(e) if e.code() == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND.to_hresult() => Ok(()),
                other => other,
            }
        };
        let _ = RegCloseKey(key);
        result
    }
}

/// Which screen corner the window sits in: the one beside the tray.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    BottomRight,
    BottomLeft,
    TopRight,
}

/// The work area (screen minus taskbar) of the primary monitor in physical
/// pixels as (left, top, right, bottom), the corner nearest the tray, and
/// the system scale factor (1.0 at 96 DPI).
pub fn tray_corner() -> Option<((i32, i32, i32, i32), Corner, f32)> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::HiDpi::GetDpiForSystem;
    use windows::Win32::UI::Shell::{ABE_LEFT, ABE_TOP, ABM_GETTASKBARPOS, APPBARDATA, SHAppBarMessage};
    use windows::Win32::UI::WindowsAndMessaging::{SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW};
    // SAFETY: both calls fill plain structs we own and size correctly.
    unsafe {
        let mut work = RECT::default();
        SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut work as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0))
            .ok()?;
        let mut bar = APPBARDATA { cbSize: std::mem::size_of::<APPBARDATA>() as u32, ..Default::default() };
        let corner = if SHAppBarMessage(ABM_GETTASKBARPOS, &mut bar) != 0 {
            match bar.uEdge {
                e if e == ABE_TOP => Corner::TopRight,
                e if e == ABE_LEFT => Corner::BottomLeft,
                _ => Corner::BottomRight,
            }
        } else {
            Corner::BottomRight
        };
        let scale = GetDpiForSystem() as f32 / 96.0;
        Some(((work.left, work.top, work.right, work.bottom), corner, scale))
    }
}

/// Top-left (logical points, as eframe wants) for a window of `size` logical
/// points sitting in the tray corner with a small gap.
pub fn default_position(size: [f32; 2]) -> Option<[f32; 2]> {
    const GAP: f32 = 12.0;
    let ((l, t, r, b), corner, scale) = tray_corner()?;
    let (l, t, r, b) = (l as f32 / scale, t as f32 / scale, r as f32 / scale, b as f32 / scale);
    let x = match corner {
        Corner::BottomLeft => l + GAP,
        _ => r - GAP - size[0],
    };
    let y = match corner {
        Corner::TopRight => t + GAP,
        _ => b - GAP - size[1],
    };
    Some([x, y])
}
