//! Global hotkeys via `RegisterHotKey` (FR-6). Chords come from `crate::chord`.
//! Hotkey id = binding index + 1, delivered as `WM_HOTKEY` to the app window.
//! Key repeat from a held key repeats the action (no `MOD_NOREPEAT`, FR-6.6).

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, RegisterHotKey, UnregisterHotKey,
};

use crate::chord::Chord;

pub struct Hotkeys {
    hwnd: HWND,
    registered: Vec<i32>,
}

impl Hotkeys {
    pub fn new(hwnd: HWND) -> Self {
        Self {
            hwnd,
            registered: Vec::new(),
        }
    }

    /// Replace all registrations. Returns the chords that couldn't be
    /// registered (usually: another app owns them) with the OS error.
    pub fn register_all(&mut self, chords: &[Chord]) -> Vec<(Chord, String)> {
        self.unregister_all();
        let mut failed = Vec::new();
        for (i, chord) in chords.iter().enumerate() {
            let id = i as i32 + 1;
            // SAFETY: plain Win32 call with our own window handle.
            match unsafe {
                RegisterHotKey(Some(self.hwnd), id, HOT_KEY_MODIFIERS(chord.mods), chord.vk)
            } {
                Ok(()) => self.registered.push(id),
                Err(e) => failed.push((*chord, e.message())),
            }
        }
        failed
    }

    pub fn unregister_all(&mut self) {
        for id in self.registered.drain(..) {
            // SAFETY: unregistering an id we registered on this window.
            let _ = unsafe { UnregisterHotKey(Some(self.hwnd), id) };
        }
    }

    /// Binding index for a `WM_HOTKEY` wparam.
    pub fn index_of(id: usize) -> Option<usize> {
        id.checked_sub(1)
    }
}

impl Drop for Hotkeys {
    fn drop(&mut self) {
        self.unregister_all();
    }
}
