//! Cross-process toggle signal (Named Event).
//! More reliable than pipe-only push for hotkey → UI show.

use tracing::{info, warn};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Threading::{CreateEventW, SetEvent};

pub const TOGGLE_EVENT_NAME: &str = "Local\\SparkLauncherToggle_v1";

/// Pulse the named auto-reset event so UI waiters wake.
pub fn signal_toggle() {
    let name: Vec<u16> = TOGGLE_EVENT_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // bManualReset=false, bInitialState=false → auto-reset
    let handle = unsafe { CreateEventW(None, false, false, PCWSTR(name.as_ptr())) };
    match handle {
        Ok(h) if !h.is_invalid() => {
            let ok = unsafe { SetEvent(h) };
            if ok.is_err() {
                warn!("SetEvent toggle failed");
            } else {
                info!("toggle event signaled");
            }
            unsafe {
                let _ = CloseHandle(h);
            }
        }
        Ok(_) => warn!("CreateEvent invalid handle"),
        Err(e) => warn!(?e, "CreateEvent toggle failed"),
    }
}

#[allow(dead_code)]
pub fn event_handle() -> windows::core::Result<HANDLE> {
    let name: Vec<u16> = TOGGLE_EVENT_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe { CreateEventW(None, false, false, PCWSTR(name.as_ptr())) }
}
