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
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, GetThreadDesktop, GetUserObjectInformationW, OpenInputDesktop, SetThreadDesktop,
    DESKTOP_ACCESS_FLAGS, HDESK, UOI_NAME,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    DrawIconEx, GetCursorInfo, GetIconInfo, CURSORINFO, CURSOR_SHOWING, DI_NORMAL, ICONINFO,
};

use crate::capture::{get_display_bounds, DisplayBounds};

fn desktop_name(hdesk: HDESK) -> Option<String> {
    if hdesk.is_invalid() {
        return None;
    }
    let mut buf = [0u16; 128];
    let mut needed = 0u32;
    let ok = unsafe {
        GetUserObjectInformationW(
            windows::Win32::Foundation::HANDLE(hdesk.0),
            UOI_NAME,
            Some(buf.as_mut_ptr() as *mut c_void),
            (buf.len() * mem::size_of::<u16>()) as u32,
            Some(&mut needed),
        )
        .is_ok()
    };
    if !ok {
        return None;
    }
    let len = (needed as usize / 2).min(buf.len());
    Some(
        String::from_utf16_lossy(&buf[..len])
            .trim_matches('\0')
            .to_string(),
    )
}

/// Attach the current OS thread to Windows' active input desktop (`Default`, `Winlogon` lock/login
/// screen, or UAC secure desktop). Returns `true` if the thread switched to a different desktop.
pub(crate) fn sync_thread_to_input_desktop() -> bool {
    unsafe {
        let Ok(input_desk) = OpenInputDesktop(
            windows::Win32::System::StationsAndDesktops::DESKTOP_CONTROL_FLAGS(0),
            false,
            DESKTOP_ACCESS_FLAGS(0x01FF),
        ) else {
            return false;
        };
        if input_desk.is_invalid() {
            return false;
        }
        let cur_name = GetThreadDesktop(GetCurrentThreadId())
            .ok()
            .and_then(desktop_name);
        let same = match (cur_name, desktop_name(input_desk)) {
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(&b),
            _ => false,
        };
        if same {
            let _ = CloseDesktop(input_desk);
            return false;
        }
        let switched = SetThreadDesktop(input_desk).is_ok();
        let _ = CloseDesktop(input_desk);
        switched
    }
}

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

    /// Capture the monitor directly into the persistent BGRA DIB buffer.
    /// When `draw_cursor` is true (e.g. Android Cursor/Trackpad mode), overlays the real
    /// Windows system cursor (arrow, hand, I-beam, resize, etc.) at its exact hotspot.
    pub fn capture_bgra(&mut self, draw_cursor: bool) -> Result<(&[u8], usize, usize)> {
        unsafe {
            // Switch capture thread to active input desktop (Winlogon lock screen / UAC / Default)
            if sync_thread_to_input_desktop() {
                let _ = ReleaseDC(None, self.screen_dc);
                let new_dc = GetDC(None);
                if !new_dc.is_invalid() {
                    self.screen_dc = new_dc;
                }
            }

            if BitBlt(
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
            .is_err()
            {
                // Re-acquire desktop DC once on transient desktop switch / lock screen transition
                let _ = ReleaseDC(None, self.screen_dc);
                let new_dc = GetDC(None);
                if !new_dc.is_invalid() {
                    self.screen_dc = new_dc;
                }
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
            }

            if draw_cursor {
                let mut ci = CURSORINFO {
                    cbSize: mem::size_of::<CURSORINFO>() as u32,
                    ..Default::default()
                };
                if GetCursorInfo(&mut ci).is_ok()
                    && (ci.flags.0 & CURSOR_SHOWING.0) != 0
                    && !ci.hCursor.is_invalid()
                {
                    let mut x = ci.ptScreenPos.x - self.bounds.x;
                    let mut y = ci.ptScreenPos.y - self.bounds.y;
                    let mut ii = ICONINFO::default();
                    if GetIconInfo(ci.hCursor.into(), &mut ii).is_ok() {
                        x -= ii.xHotspot as i32;
                        y -= ii.yHotspot as i32;
                        if !ii.hbmMask.is_invalid() {
                            let _ = DeleteObject(HGDIOBJ(ii.hbmMask.0));
                        }
                        if !ii.hbmColor.is_invalid() {
                            let _ = DeleteObject(HGDIOBJ(ii.hbmColor.0));
                        }
                    }
                    let _ = DrawIconEx(
                        self.mem_dc,
                        x,
                        y,
                        ci.hCursor.into(),
                        0,
                        0,
                        0,
                        None,
                        DI_NORMAL,
                    );
                }
            }

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
