//! Persistent, zero-allocation Windows GDI screen capturer (`CreateDIBSection` + `BitBlt`).
//!
//! Advantages over both `xcap` and DXGI Desktop Duplication for remote desktop:
//! - **No remote cursor**: `BitBlt(SRCCOPY)` never bakes the host mouse pointer into the
//!   captured frame, so the viewer sees only their own zero-latency local cursor.
//! - **Zero per-frame allocations**: Reuses a single `HDC` + `HBITMAP` DIB section (no 8 MB
//!   heap alloc/free or `GetDIBits` copy per frame, and no `BGRA -> RGBA` swizzle pass).
//! - **Rock-solid**: Never fails with `DXGI_ERROR_ACCESS_LOST` on UAC/resolution/GPU switches.

use std::ffi::c_void;
use std::mem;

use anyhow::{anyhow, Result};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC,
    SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
    SRCCOPY,
};

use crate::capture::{get_display_bounds, DisplayBounds};

pub struct WinGdiCapturer {
    bounds: DisplayBounds,
    screen_dc: HDC,
    mem_dc: HDC,
    bitmap: HBITMAP,
    old_obj: HGDIOBJ,
    bits_ptr: *const u8,
    byte_len: usize,
}

// SAFETY: `WinGdiCapturer` is only ever owned and used by the single capture OS thread.
unsafe impl Send for WinGdiCapturer {}

impl WinGdiCapturer {
    pub fn new(display_index: usize) -> Result<Self> {
        let bounds = get_display_bounds(display_index)?;
        if bounds.width == 0 || bounds.height == 0 {
            return Err(anyhow!("Display {} has zero dimensions", display_index));
        }

        unsafe {
            let screen_dc = GetDC(None);
            if screen_dc.is_invalid() {
                return Err(anyhow!("GetDC(NULL) failed"));
            }

            let mem_dc = CreateCompatibleDC(Some(screen_dc));
            if mem_dc.is_invalid() {
                let _ = ReleaseDC(None, screen_dc);
                return Err(anyhow!("CreateCompatibleDC failed"));
            }

            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: bounds.width as i32,
                    biHeight: -(bounds.height as i32), // top-down DIB
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    biSizeImage: 0,
                    ..Default::default()
                },
                ..Default::default()
            };

            let mut bits: *mut c_void = std::ptr::null_mut();
            let bitmap = match CreateDIBSection(
                Some(mem_dc),
                &bmi,
                DIB_RGB_COLORS,
                &mut bits,
                None,
                0,
            ) {
                Ok(bmp) if !bmp.is_invalid() && !bits.is_null() => bmp,
                Ok(_) => {
                    let _ = DeleteDC(mem_dc);
                    let _ = ReleaseDC(None, screen_dc);
                    return Err(anyhow!("CreateDIBSection returned null bitmap/bits"));
                }
                Err(e) => {
                    let _ = DeleteDC(mem_dc);
                    let _ = ReleaseDC(None, screen_dc);
                    return Err(anyhow!("CreateDIBSection failed: {:?}", e));
                }
            };

            let old_obj = SelectObject(mem_dc, HGDIOBJ(bitmap.0));
            let byte_len = (bounds.width as usize) * (bounds.height as usize) * 4;

            Ok(Self {
                bounds,
                screen_dc,
                mem_dc,
                bitmap,
                old_obj,
                bits_ptr: bits as *const u8,
                byte_len,
            })
        }
    }

    /// Capture the monitor directly into the persistent BGRA DIB buffer (without the host cursor).
    pub fn capture_bgra(&mut self) -> Result<(&[u8], usize, usize)> {
        unsafe {
            BitBlt(
                self.mem_dc,
                0,
                0,
                self.bounds.width as i32,
                self.bounds.height as i32,
                Some(self.screen_dc),
                self.bounds.x,
                self.bounds.y,
                SRCCOPY,
            )
            .map_err(|e| anyhow!("BitBlt failed: {:?}", e))?;

            let slice = std::slice::from_raw_parts(self.bits_ptr, self.byte_len);
            Ok((slice, self.bounds.width as usize, self.bounds.height as usize))
        }
    }
}

impl Drop for WinGdiCapturer {
    fn drop(&mut self) {
        unsafe {
            if !self.mem_dc.is_invalid() {
                if !self.old_obj.is_invalid() {
                    let _ = SelectObject(self.mem_dc, self.old_obj);
                }
                if !self.bitmap.is_invalid() {
                    let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
                }
                let _ = DeleteDC(self.mem_dc);
            }
            if !self.screen_dc.is_invalid() {
                let _ = ReleaseDC(None, self.screen_dc);
            }
        }
    }
}
