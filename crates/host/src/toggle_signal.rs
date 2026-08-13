//! Cross-process toggle signal (Named Event).
//! More reliable than pipe-only push for hotkey → UI show.

use tracing::{info, warn};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Threading::{CreateEventW, SetEvent};

pub const TOGGLE_EVENT_NAME: &str = "Local\\SparkLauncherToggle_v1";
/// 退出信号：host 托盘"退出"/host.exit 时 SetEvent，UI 的 ExitWatcher 收到后整个应用退出。
/// 不用 pipe 广播：host 的管道句柄是同步的，client_read_loop 的 ReadFile 挂起时
/// 同句柄 WriteFile 会被阻塞到读方向返回（实测 11-30s 延迟），命名事件无此问题。
pub const EXIT_EVENT_NAME: &str = "Local\\SparkLauncherExit_v1";

/// Pulse the named auto-reset event so UI waiters wake.
pub fn signal_toggle() {
    signal_event(TOGGLE_EVENT_NAME, "toggle");
}

/// Signal the named exit event so the UI shuts down together with the host.
pub fn signal_exit() {
    signal_event(EXIT_EVENT_NAME, "exit");
}

fn signal_event(name: &str, kind: &str) {
    let name: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // bManualReset=false, bInitialState=false → auto-reset
    let handle = unsafe { CreateEventW(None, false, false, PCWSTR(name.as_ptr())) };
    match handle {
        Ok(h) if !h.is_invalid() => {
            let ok = unsafe { SetEvent(h) };
            if ok.is_err() {
                warn!(kind, "SetEvent failed");
            } else {
                info!(kind, "event signaled");
            }
            unsafe {
                let _ = CloseHandle(h);
            }
        }
        Ok(_) => warn!(kind, "CreateEvent invalid handle"),
        Err(e) => warn!(?e, kind, "CreateEvent failed"),
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
