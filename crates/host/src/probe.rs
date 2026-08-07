//! One-shot diagnostics: is the spark launcher window visible?
//! `spark-host --probe-ui`（继承当前控制台，不会创建新窗口抢前台，
//! 适合无人值守验证"窗口显示后是否保持"）。

use anyhow::Result;
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowTextW, IsWindowVisible};

pub fn print_ui_visible() -> Result<()> {
    let mut found = false;
    // 回调返回 FALSE（找到窗口提前停止）时封装层会报 E_HANDLE，属正常停止，忽略。
    let _ = unsafe {
        EnumWindows(
            Some(enum_proc),
            LPARAM(std::ptr::addr_of_mut!(found) as isize),
        )
    };
    if !found {
        println!("Spark window: NOT FOUND");
    }
    Ok(())
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let mut buf = [0u16; 256];
    let n = GetWindowTextW(hwnd, &mut buf);
    let title = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
    if title == "Spark" {
        let visible = IsWindowVisible(hwnd).as_bool();
        println!("Spark window: {hwnd:?} visible={visible}");
        *(lparam.0 as *mut bool) = true;
        BOOL(0) // stop enumerating
    } else {
        BOOL(1)
    }
}
