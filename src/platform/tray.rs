//! Tray icon (FR-8.1) via `Shell_NotifyIconW`. The icon (a volume dial, see
//! `crate::icon`) is drawn at runtime, so no resource file or build script is
//! needed: orange when connected, grey when Sonar is unavailable.

use windows::Win32::Foundation::{HWND, POINT};

use crate::icon;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    DeleteObject, HGDIOBJ,
};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_INFO, NIIF_WARNING, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NOTIFY_ICON_DATA_FLAGS, NOTIFYICON_VERSION_4,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, GetCursorPos, GetSystemMetrics, HICON, ICONINFO, SM_CXSMICON,
};

const ICON_ID: u32 = 1;

pub struct Tray {
    hwnd: HWND,
    callback_msg: u32,
    icon_on: Option<HICON>,
    icon_off: Option<HICON>,
    connected: bool,
    tip: String,
}

/// Copy `s` into a fixed UTF-16 buffer, truncating and NUL-terminating.
fn fill<const N: usize>(dst: &mut [u16; N], s: &str) {
    let mut i = 0;
    for u in s.encode_utf16().take(N - 1) {
        dst[i] = u;
        i += 1;
    }
    dst[i] = 0;
}

/// Build an `HICON` from the dial drawn by `crate::icon`.
fn make_icon(size: i32, rgb: (u8, u8, u8)) -> Option<HICON> {
    let n = size.max(16);
    let px = icon::dial(n, rgb);
    // SAFETY: standard 32-bpp top-down DIB; `bits` is valid for n*n u32s
    // until the bitmap is deleted, and both bitmaps are deleted after the
    // icon (which copies them) is created.
    unsafe {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: n,
                biHeight: -n,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let color = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        std::ptr::copy_nonoverlapping(px.as_ptr(), bits.cast::<u32>(), px.len());
        let mask = CreateBitmap(n, n, 1, 1, None);
        let info = ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&info).ok();
        let _ = DeleteObject(HGDIOBJ(color.0));
        let _ = DeleteObject(HGDIOBJ(mask.0));
        icon
    }
}

impl Tray {
    pub fn new(hwnd: HWND, callback_msg: u32) -> Self {
        // SAFETY: plain metric query.
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) };
        Self {
            hwnd,
            callback_msg,
            icon_on: make_icon(size, icon::CONNECTED),
            icon_off: make_icon(size, icon::OFFLINE),
            connected: false,
            tip: "Sonar Technician".to_owned(),
        }
    }

    fn data(&self, flags: NOTIFY_ICON_DATA_FLAGS) -> NOTIFYICONDATAW {
        let mut d = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: ICON_ID,
            uFlags: flags,
            uCallbackMessage: self.callback_msg,
            hIcon: (if self.connected {
                self.icon_on
            } else {
                self.icon_off
            })
            .unwrap_or_default(),
            ..Default::default()
        };
        fill(&mut d.szTip, &self.tip);
        d
    }

    /// Add the icon (also after Explorer restarts: `TaskbarCreated`).
    pub fn add(&self) {
        let mut d = self.data(NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP);
        // SAFETY: `d` is a fully initialised NOTIFYICONDATAW.
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &d);
            if !Shell_NotifyIconW(NIM_ADD, &d).as_bool() {
                tracing::warn!("couldn't add the tray icon");
            }
            d.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &d);
        }
    }

    pub fn update(&mut self, tip: &str, connected: bool) {
        if tip == self.tip && connected == self.connected {
            return;
        }
        self.tip = tip.to_owned();
        self.connected = connected;
        let d = self.data(NIF_ICON | NIF_TIP | NIF_SHOWTIP);
        // SAFETY: `d` is a fully initialised NOTIFYICONDATAW.
        let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &d) };
    }

    /// Windows notification ("balloon"). Used for problems the user must see
    /// even with the overlay off (hotkeys taken, config errors).
    pub fn notify(&self, title: &str, text: &str, warning: bool) {
        let mut d = self.data(NIF_INFO);
        fill(&mut d.szInfoTitle, title);
        fill(&mut d.szInfo, text);
        d.dwInfoFlags = if warning { NIIF_WARNING } else { NIIF_INFO };
        // SAFETY: `d` is a fully initialised NOTIFYICONDATAW.
        let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &d) };
    }

    pub fn remove(&self) {
        let d = self.data(NOTIFY_ICON_DATA_FLAGS(0));
        // SAFETY: `d` identifies our icon.
        let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &d) };
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.remove();
        for icon in [self.icon_on, self.icon_off].into_iter().flatten() {
            // SAFETY: icons were created by CreateIconIndirect and are destroyed once.
            let _ = unsafe { DestroyIcon(icon) };
        }
    }
}

pub fn cursor_pos() -> POINT {
    let mut p = POINT::default();
    // SAFETY: valid out-pointer.
    let _ = unsafe { GetCursorPos(&mut p) };
    p
}
