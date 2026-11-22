//! The overlay window (FR-7): layered, topmost, click-through, never
//! activated, per-monitor DPI aware. Content is a premultiplied 32-bpp bitmap
//! pushed with `UpdateLayeredWindow`, so updates during a fast knob spin
//! replace the image in place (no flicker), and fading only changes the
//! constant alpha.
//!
//! Exclusive-fullscreen games are drawn above any window; the overlay can't
//! appear over them (borderless/windowed works).

use anyhow::{Context, Result};
use windows::Win32::Foundation::{
    COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER,
    BLENDFUNCTION, CLIP_DEFAULT_PRECIS, CreateCompatibleDC, CreateDIBSection, CreateFontW,
    DEFAULT_CHARSET, DIB_RGB_COLORS, DRAW_TEXT_FORMAT, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX,
    DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DeleteDC, DeleteObject, DrawTextW, FW_NORMAL, FW_SEMIBOLD,
    GetDC, GetMonitorInfoW, HDC, HFONT, HGDIOBJ, MONITOR_DEFAULTTONEAREST,
    MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint, OUT_DEFAULT_PRECIS, ReleaseDC,
    SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, HTTRANSPARENT, HWND_TOPMOST, MA_NOACTIVATE, RegisterClassW,
    SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos, ShowWindow,
    ULW_ALPHA, UpdateLayeredWindow, WM_MOUSEACTIVATE, WM_NCHITTEST, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{PCWSTR, w};

use crate::config::{MonitorChoice, OverlayConfig};
use crate::overlay_view::{ACCENT, Canvas, DIM, Layout, MUTED, Rect, TEXT, View, layout, place};
use crate::platform::tray::cursor_pos;

const CLASS: PCWSTR = w!("SonarTechnicianOverlay");

extern "system" fn overlay_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // Click-through even where WS_EX_TRANSPARENT isn't honoured.
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        // SAFETY: default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

pub struct Overlay {
    hwnd: HWND,
    visible: bool,
    /// Alpha for the current show (config opacity).
    full_alpha: u8,
}

fn rgb(c: (u8, u8, u8)) -> COLORREF {
    COLORREF(u32::from(c.0) | (u32::from(c.1) << 8) | (u32::from(c.2) << 16))
}

/// Work area and effective DPI scale of the chosen monitor.
fn monitor(choice: MonitorChoice) -> (Rect, f32) {
    // SAFETY: plain monitor queries with valid out-pointers.
    unsafe {
        let hmon = match choice {
            MonitorChoice::Primary => {
                MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY)
            }
            MonitorChoice::Cursor => MonitorFromPoint(cursor_pos(), MONITOR_DEFAULTTONEAREST),
        };
        let mut mi = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(hmon, &mut mi);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        let w = mi.rcWork;
        (
            Rect {
                left: w.left,
                top: w.top,
                right: w.right,
                bottom: w.bottom,
            },
            dx as f32 / 96.0,
        )
    }
}

/// GDI font (deleted by the caller).
fn font(px: i32, weight: u32) -> HFONT {
    // SAFETY: plain font creation.
    unsafe {
        CreateFontW(
            -px,
            0,
            0,
            0,
            weight as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            0,
            w!("Segoe UI"),
        )
    }
}

/// Draw one line of text into `rect` (x0, y, x1, h).
fn text(
    dc: HDC,
    s: &str,
    (x0, top, x1, h): (i32, i32, i32, i32),
    color: (u8, u8, u8),
    right: bool,
) {
    let mut buf: Vec<u16> = s.encode_utf16().collect();
    if buf.is_empty() {
        return;
    }
    let mut r = RECT {
        left: x0,
        top,
        right: x1,
        bottom: top + h,
    };
    let align: DRAW_TEXT_FORMAT = if right { DT_RIGHT } else { DT_LEFT };
    // SAFETY: `dc` is a valid memory DC with a font selected; `buf` and `r` outlive the call.
    unsafe {
        SetTextColor(dc, rgb(color));
        DrawTextW(
            dc,
            &mut buf,
            &mut r,
            align | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
    }
}

impl Overlay {
    pub fn new(hinstance: HINSTANCE) -> Result<Self> {
        // SAFETY: standard class registration and window creation.
        let hwnd = unsafe {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(overlay_proc),
                hInstance: hinstance,
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassW(&wc);
            CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE,
                CLASS,
                w!("Sonar Technician overlay"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(hinstance),
                None,
            )
            .context("creating the overlay window")?
        };
        Ok(Self {
            hwnd,
            visible: false,
            full_alpha: 230,
        })
    }

    /// Render `view` and show it at full opacity (in place if already shown).
    pub fn show(&mut self, view: &View, cfg: &OverlayConfig) {
        let (work, dpi_scale) = monitor(cfg.monitor);
        let l = layout(
            dpi_scale * cfg.scale,
            view.level.is_some(),
            view.footer.is_some(),
        );
        let offset = (
            (cfg.offset.0 as f32 * dpi_scale).round() as i32,
            (cfg.offset.1 as f32 * dpi_scale).round() as i32,
        );
        let (x, y) = place(work, (l.width, l.height), cfg.position, offset);
        self.full_alpha = (cfg.opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
        if let Err(e) = self.render(view, &l, (x, y)) {
            tracing::warn!("overlay render failed: {e}");
            return;
        }
        if !self.visible {
            self.visible = true;
            // SAFETY: showing our own window without activating it.
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
        }
        // Re-assert topmost (other topmost windows may have come up since).
        // SAFETY: z-order change only; never activates.
        let _ = unsafe {
            SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )
        };
    }

    fn render(&self, v: &View, l: &Layout, (x, y): (i32, i32)) -> Result<()> {
        let mut canvas = Canvas::panel(l);
        if let Some(level) = v.level {
            canvas.bar(l, level, v.muted);
        }
        let (w, h) = (l.width, l.height);
        // SAFETY: standard layered-window update: a 32-bpp top-down DIB in a
        // memory DC; every GDI object created here is released below, and
        // `bits` is only used while the DIB is alive.
        unsafe {
            let screen = GetDC(None);
            let mem = CreateCompatibleDC(Some(screen));
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let dib = CreateDIBSection(Some(mem), &bmi, DIB_RGB_COLORS, &mut bits, None, 0);
            let result = match dib {
                Ok(dib) => {
                    let n = (w * h) as usize;
                    let pixels = std::slice::from_raw_parts_mut(bits.cast::<u32>(), n);
                    pixels.copy_from_slice(&canvas.px);
                    let old = SelectObject(mem, HGDIOBJ(dib.0));
                    SetBkMode(mem, TRANSPARENT);

                    let small = font(l.small_font, FW_NORMAL.0);
                    let big = font(l.title_font, FW_SEMIBOLD.0);
                    let (x0, x1) = (l.pad, w - l.pad);
                    let line = |(top, hh): (i32, i32)| (x0, top, x1, hh);

                    SelectObject(mem, HGDIOBJ(small.0));
                    text(mem, &v.header, line(l.header), DIM, false);
                    if v.muted {
                        text(mem, "MUTED", line(l.header), MUTED, true);
                    }
                    if let (Some(f), Some(pos)) = (&v.footer, l.footer) {
                        text(mem, f, line(pos), DIM, false);
                    }
                    SelectObject(mem, HGDIOBJ(big.0));
                    // Leave room for the value on the right of the title.
                    let value_w = if v.value.is_some() {
                        l.title_font * 3
                    } else {
                        0
                    };
                    text(
                        mem,
                        &v.title,
                        (x0, l.title.0, x1 - value_w, l.title.1),
                        TEXT,
                        false,
                    );
                    if let Some(val) = &v.value {
                        let c = if v.muted { DIM } else { ACCENT };
                        text(mem, val, line(l.title), c, true);
                    }

                    canvas.px.copy_from_slice(pixels);
                    canvas.finish();
                    pixels.copy_from_slice(&canvas.px);

                    let blend = BLENDFUNCTION {
                        BlendOp: AC_SRC_OVER as u8,
                        BlendFlags: 0,
                        SourceConstantAlpha: self.full_alpha,
                        AlphaFormat: AC_SRC_ALPHA as u8,
                    };
                    let r = UpdateLayeredWindow(
                        self.hwnd,
                        Some(screen),
                        Some(&POINT { x, y }),
                        Some(&SIZE { cx: w, cy: h }),
                        Some(mem),
                        Some(&POINT { x: 0, y: 0 }),
                        COLORREF(0),
                        Some(&blend),
                        ULW_ALPHA,
                    );
                    SelectObject(mem, old);
                    let _ = DeleteObject(HGDIOBJ(small.0));
                    let _ = DeleteObject(HGDIOBJ(big.0));
                    let _ = DeleteObject(HGDIOBJ(dib.0));
                    r.context("UpdateLayeredWindow")
                }
                Err(e) => Err(anyhow::anyhow!("CreateDIBSection: {e}")),
            };
            let _ = DeleteDC(mem);
            ReleaseDC(None, screen);
            result
        }
    }

    /// Set the fade level: 1.0 = full (config opacity), 0.0 = hidden.
    pub fn set_fade(&mut self, level: f32) {
        if !self.visible {
            return;
        }
        if level <= 0.0 {
            self.hide();
            return;
        }
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: (f32::from(self.full_alpha) * level.min(1.0)).round() as u8,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        // SAFETY: alpha-only update of our layered window (no new content).
        let _ = unsafe {
            UpdateLayeredWindow(
                self.hwnd,
                None,
                None,
                None,
                None,
                None,
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
        };
    }

    pub fn hide(&mut self) {
        if self.visible {
            self.visible = false;
            // SAFETY: hiding our own window.
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
    }
}
