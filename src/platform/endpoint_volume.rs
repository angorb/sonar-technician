//! Core Audio endpoint-volume writer for Sonar's virtual devices (FR-3.7).
//!
//! REST volume/mute writes change Sonar's audio but not the GG UI (confirmed
//! GG 124.0.0). Setting the Windows endpoint volume of a channel's virtual
//! device ("SteelSeries Sonar - Gaming", ...) is reported to make Sonar mirror
//! the value into its fader. Classic mode only; master has no virtual device.
//!
//! Device IDs from Sonar's `/audioDevices` are MMDevice endpoint IDs
//! (`{0.0.0.00000000}.{guid}`), so they're passed to `GetDevice` directly.
//!
//! COM objects aren't `Send`: create an [`EndpointVolume`] on the thread that
//! uses it (the Sonar worker).

use thiserror::Error;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::core::HSTRING;

#[derive(Debug, Error)]
pub enum EndpointError {
    #[error("Core Audio {op} failed: {source}")]
    Com {
        op: &'static str,
        source: windows::core::Error,
    },
    #[error("invalid volume {0} (must be 0.0..=1.0)")]
    InvalidVolume(f32),
}

fn com(op: &'static str) -> impl FnOnce(windows::core::Error) -> EndpointError {
    move |source| EndpointError::Com { op, source }
}

/// Balances a successful `CoInitializeEx` on drop.
struct ComGuard {
    initialized: bool,
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: paired with the successful CoInitializeEx in `new` on this thread.
            unsafe { CoUninitialize() };
        }
    }
}

pub struct EndpointVolume {
    enumerator: IMMDeviceEnumerator,
    // Field order matters: the enumerator must be released before COM is
    // uninitialised, and fields drop in declaration order.
    _com: ComGuard,
}

impl EndpointVolume {
    pub fn new() -> Result<Self, EndpointError> {
        // SAFETY: plain FFI call; the result is checked. S_FALSE (already
        // initialised on this thread) still needs a matching CoUninitialize.
        // RPC_E_CHANGED_MODE (thread already STA) is fine to use as-is but
        // must not be uninitialised by us.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let guard = ComGuard {
            initialized: hr.is_ok(),
        };
        // SAFETY: COM is initialised on this thread (or was already).
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
                .map_err(com("create MMDeviceEnumerator"))?;
        Ok(Self {
            enumerator,
            _com: guard,
        })
    }

    fn endpoint(&self, device_id: &str) -> Result<IAudioEndpointVolume, EndpointError> {
        let id = HSTRING::from(device_id);
        // SAFETY: `id` outlives the call; returned interfaces are ref-counted.
        unsafe {
            let device = self.enumerator.GetDevice(&id).map_err(com("GetDevice"))?;
            device
                .Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
                .map_err(com("Activate IAudioEndpointVolume"))
        }
    }

    /// Windows volume scalar (0.0..=1.0), as shown in the Windows mixer.
    pub fn volume(&self, device_id: &str) -> Result<f32, EndpointError> {
        let ep = self.endpoint(device_id)?;
        // SAFETY: valid interface from `endpoint`.
        unsafe { ep.GetMasterVolumeLevelScalar() }.map_err(com("GetMasterVolumeLevelScalar"))
    }

    pub fn set_volume(&self, device_id: &str, volume: f32) -> Result<(), EndpointError> {
        if !volume.is_finite() || !(0.0..=1.0).contains(&volume) {
            return Err(EndpointError::InvalidVolume(volume));
        }
        let ep = self.endpoint(device_id)?;
        // SAFETY: valid interface; a null event-context GUID is allowed.
        unsafe { ep.SetMasterVolumeLevelScalar(volume, std::ptr::null()) }
            .map_err(com("SetMasterVolumeLevelScalar"))
    }

    pub fn muted(&self, device_id: &str) -> Result<bool, EndpointError> {
        let ep = self.endpoint(device_id)?;
        // SAFETY: valid interface from `endpoint`.
        unsafe { ep.GetMute() }
            .map(|b| b.as_bool())
            .map_err(com("GetMute"))
    }

    pub fn set_mute(&self, device_id: &str, muted: bool) -> Result<(), EndpointError> {
        let ep = self.endpoint(device_id)?;
        // SAFETY: valid interface; a null event-context GUID is allowed.
        unsafe { ep.SetMute(muted, std::ptr::null()) }.map_err(com("SetMute"))
    }
}
