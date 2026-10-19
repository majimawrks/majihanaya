//! Puts rendered pixels on screen through a Win32 *layered* window: a
//! top-down 32-bit premultiplied BGRA DIB handed to `UpdateLayeredWindow`.
//! That gives per-pixel alpha (rounded corners, click-through outside the
//! rack), window opacity as one constant, and resizing + moving in the same
//! call, so the tray-corner anchor never flickers.

use windows::Win32::Foundation::{COLORREF, HWND, POINT, SIZE};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, CreateCompatibleDC,
    CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HBITMAP, HDC, HGDIOBJ, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetWindowLongPtrW, SetWindowLongPtrW, ULW_ALPHA, UpdateLayeredWindow, WS_EX_LAYERED,
};

pub struct Presenter {
    hwnd: HWND,
    dc: HDC,
    dib: Option<(HBITMAP, HGDIOBJ, *mut u32)>,
    w: usize,
    h: usize,
}

impl Presenter {
    /// Turns `hwnd` into a layered window.
    pub fn new(hwnd: isize) -> Self {
        let hwnd = HWND(hwnd as *mut _);
        // SAFETY: our own window; adding a style bit and creating a memory DC.
        let dc = unsafe {
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_LAYERED.0 as isize);
            CreateCompatibleDC(None)
        };
        Self { hwnd, dc, dib: None, w: 0, h: 0 }
    }

    /// Makes the back buffer `w`×`h`. `Ok(true)` when it was recreated (its
    /// contents are then undefined: redraw everything). `Err` when Windows
    /// can't allocate it; the previous buffer is kept and stays presentable.
    pub fn ensure_size(&mut self, w: usize, h: usize) -> Result<bool, ()> {
        let (w, h) = (w.max(1), h.max(1));
        if self.dib.is_some() && (w, h) == (self.w, self.h) {
            return Ok(false);
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32), // top-down rows
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        // SAFETY: valid header; `bits` receives the DIB memory, owned by the
        // bitmap until `release` deletes it.
        unsafe {
            let bmp = CreateDIBSection(Some(self.dc), &info, DIB_RGB_COLORS, &mut bits, None, 0).map_err(|_| ())?;
            if bits.is_null() {
                let _ = DeleteObject(bmp.into());
                return Err(());
            }
            // Only now drop the old buffer, then select the new one.
            self.release();
            let old = SelectObject(self.dc, bmp.into());
            self.dib = Some((bmp, old, bits.cast()));
            self.w = w;
            self.h = h;
        }
        Ok(true)
    }

    pub fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }

    /// The back buffer, premultiplied `0xAARRGGBB` rows, top-down.
    pub fn buffer(&mut self) -> &mut [u32] {
        match self.dib {
            // SAFETY: the DIB holds exactly w*h u32 pixels and lives until
            // `release`; `&mut self` keeps this the only view.
            Some((_, _, bits)) => unsafe { std::slice::from_raw_parts_mut(bits, self.w * self.h) },
            None => &mut [],
        }
    }

    /// Shows the buffer. `pos` (physical px) moves the window in the same
    /// update; `opacity` multiplies every pixel's alpha.
    pub fn present(&self, pos: Option<(i32, i32)>, opacity: f32) {
        if self.dib.is_none() {
            return;
        }
        let size = SIZE { cx: self.w as i32, cy: self.h as i32 };
        let src = POINT { x: 0, y: 0 };
        let dst = pos.map(|(x, y)| POINT { x, y });
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: (opacity.clamp(0.0, 1.0) * 255.0).round() as u8,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        // SAFETY: all pointers are to locals alive for the call.
        unsafe {
            let _ = UpdateLayeredWindow(
                self.hwnd,
                None,
                dst.as_ref().map(|p| p as *const _),
                Some(&size),
                Some(self.dc),
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
        }
    }

    fn release(&mut self) {
        if let Some((bmp, old, _)) = self.dib.take() {
            // SAFETY: restore the DC's original bitmap before deleting ours.
            unsafe {
                SelectObject(self.dc, old);
                let _ = DeleteObject(bmp.into());
            }
        }
    }
}

impl Drop for Presenter {
    fn drop(&mut self) {
        self.release();
        // SAFETY: the DC was created by us and no bitmap of ours is selected.
        unsafe {
            let _ = DeleteDC(self.dc);
        }
    }
}
