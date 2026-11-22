//! Single instance (FR-8.3): a named mutex; a second launch pokes the running
//! instance's window and exits.

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, LPARAM, WPARAM,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW};
use windows::core::{PCWSTR, w};

pub struct InstanceGuard(HANDLE);

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        // SAFETY: handle came from CreateMutexW and is closed once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// `None` if another instance already holds the mutex.
pub fn acquire() -> Option<InstanceGuard> {
    // SAFETY: plain Win32 call; GetLastError is read immediately after.
    unsafe {
        let handle =
            CreateMutexW(None, false, w!("Local\\sonar-technician-single-instance")).ok()?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(handle);
            return None;
        }
        Some(InstanceGuard(handle))
    }
}

/// Post `msg` to the running instance's window (found by class name).
pub fn notify_existing(class: PCWSTR, msg: u32) {
    // SAFETY: plain Win32 calls; a missing window is ignored.
    unsafe {
        if let Ok(hwnd) = FindWindowW(class, PCWSTR::null()) {
            let _ = PostMessageW(Some(hwnd), msg, WPARAM(0), LPARAM(0));
        }
    }
}
