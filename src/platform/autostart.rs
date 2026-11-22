//! "Start with Windows" via `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`
//! (FR-8.5). No admin rights needed.

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};
use windows::core::{HSTRING, w};

const RUN_KEY: windows::core::PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE: windows::core::PCWSTR = w!("sonar-technician");

pub fn is_enabled() -> bool {
    // SAFETY: query for existence only; no buffers passed.
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            VALUE,
            RRF_RT_REG_SZ,
            None,
            None,
            None,
        ) == ERROR_SUCCESS
    }
}

pub fn set(enabled: bool) -> Result<(), String> {
    if !enabled {
        // SAFETY: plain Win32 call.
        let r = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE) };
        return if r == ERROR_SUCCESS || !is_enabled() {
            Ok(())
        } else {
            Err(format!("couldn't remove autostart entry ({})", r.0))
        };
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let cmd = HSTRING::from(format!("\"{}\"", exe.display()));
    let bytes = (cmd.len() + 1) * 2; // UTF-16 incl. terminator
    // SAFETY: `cmd` is a NUL-terminated UTF-16 buffer of `bytes` bytes.
    let r = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            VALUE,
            REG_SZ.0,
            Some(cmd.as_ptr().cast()),
            bytes as u32,
        )
    };
    if r == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!("couldn't write autostart entry ({})", r.0))
    }
}
