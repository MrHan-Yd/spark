//! System notification-area icon (Shell_NotifyIconW).

use anyhow::Result;
use std::mem;
use std::path::Path;
use tracing::info;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, DestroyMenu, GetCursorPos, InsertMenuW, LoadImageW, SetForegroundWindow,
    TrackPopupMenu, HICON, IMAGE_ICON, LR_DEFAULTSIZE, LR_LOADFROMFILE, MF_BYPOSITION, MF_STRING,
    TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_LBUTTONUP, WM_RBUTTONUP,
};

use crate::hotkey::WM_SPARK_TRAY;

pub const TRAY_ID: u32 = 1;
pub const CMD_SHOW: u32 = 1001;
pub const CMD_TOGGLE_HOTKEY: u32 = 1002;
pub const CMD_EXIT: u32 = 1003;

pub struct TrayIcon {
    data: NOTIFYICONDATAW,
}

// NOTIFYICONDATAW 含 HWND/HICON 原始指针字段，本身不 Send；
// 数据只在 HostApp 的 Mutex 保护下访问，Shell_NotifyIconW 线程安全，跨线程共享安全。
unsafe impl Send for TrayIcon {}

impl TrayIcon {
    pub fn add(hwnd: HWND, tip: &str, icon_path: Option<&Path>) -> Result<Self> {
        let hicon = load_icon(icon_path)?;

        let mut data: NOTIFYICONDATAW = unsafe { mem::zeroed() };
        data.cbSize = mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = hwnd;
        data.uID = TRAY_ID;
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        data.uCallbackMessage = WM_SPARK_TRAY;
        data.hIcon = hicon;

        // szTip is 128 WCHARs in modern NOTIFYICONDATAW
        let tip_wide: Vec<u16> = tip.encode_utf16().chain(std::iter::once(0)).collect();
        let copy_len = tip_wide.len().min(data.szTip.len());
        data.szTip[..copy_len].copy_from_slice(&tip_wide[..copy_len]);

        let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &data) };
        if !ok.as_bool() {
            anyhow::bail!("Shell_NotifyIcon ADD failed");
        }
        Ok(Self { data })
    }

    /// 右下角气泡提示（Win10/11 显示为 toast 风格），如复制结果反馈。
    pub fn show_balloon(&mut self, title: &str, text: &str) {
        write_wide(&mut self.data.szInfoTitle, title);
        write_wide(&mut self.data.szInfo, text);
        self.data.dwInfoFlags = NIIF_INFO;
        self.data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_INFO;
        let ok = unsafe { Shell_NotifyIconW(NIM_MODIFY, &self.data) };
        info!(ok = ok.as_bool(), title, "tray balloon");
        // 还原标志位，避免常驻 INFO 字段干扰后续更新
        self.data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    }

    pub fn remove(&mut self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.data);
        }
    }
}

/// 把字符串写进定长 WCHAR 缓冲区（尾部补零，保证以空字符结尾）。
fn write_wide(dst: &mut [u16], s: &str) {
    let wide: Vec<u16> = s.encode_utf16().collect();
    let copy_len = wide.len().min(dst.len().saturating_sub(1));
    dst[..copy_len].copy_from_slice(&wide[..copy_len]);
    dst[copy_len] = 0;
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        self.remove();
    }
}

fn load_icon(path: Option<&Path>) -> Result<HICON> {
    if let Some(p) = path {
        if p.is_file() {
            let wide: Vec<u16> = p
                .to_string_lossy()
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let handle = unsafe {
                LoadImageW(
                    None,
                    PCWSTR(wide.as_ptr()),
                    IMAGE_ICON,
                    0,
                    0,
                    LR_LOADFROMFILE | LR_DEFAULTSIZE,
                )
            };
            if let Ok(h) = handle {
                return Ok(HICON(h.0));
            }
        }
    }
    // Fallback: IDI_APPLICATION
    let handle = unsafe {
        LoadImageW(
            None,
            windows::Win32::UI::WindowsAndMessaging::IDI_APPLICATION,
            IMAGE_ICON,
            0,
            0,
            LR_DEFAULTSIZE,
        )
    }?;
    Ok(HICON(handle.0))
}

/// Handle WM_SPARK_TRAY; returns Some(command) for menu selections.
pub fn handle_tray_message(hwnd: HWND, lparam: LPARAM, hotkey_paused: bool) -> Option<u32> {
    let msg = (lparam.0 as u32) & 0xFFFF;
    match msg {
        m if m == WM_LBUTTONUP => Some(CMD_SHOW),
        m if m == WM_RBUTTONUP => show_context_menu(hwnd, hotkey_paused),
        _ => None,
    }
}

fn show_context_menu(hwnd: HWND, hotkey_paused: bool) -> Option<u32> {
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        let _ = InsertMenuW(
            menu,
            0,
            MF_BYPOSITION | MF_STRING,
            CMD_SHOW as usize,
            w!("显示 Spark"),
        );
        let pause_label = if hotkey_paused {
            w!("恢复热键")
        } else {
            w!("暂停热键")
        };
        let _ = InsertMenuW(
            menu,
            1,
            MF_BYPOSITION | MF_STRING,
            CMD_TOGGLE_HOTKEY as usize,
            pause_label,
        );
        let _ = InsertMenuW(
            menu,
            2,
            MF_BYPOSITION | MF_STRING,
            CMD_EXIT as usize,
            w!("退出"),
        );

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_LEFTALIGN | TPM_RIGHTBUTTON | TPM_RETURNCMD,
            pt.x,
            pt.y,
            Some(0),
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);
        // Required per MSDN after TrackPopupMenu
        let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
            Some(hwnd),
            windows::Win32::UI::WindowsAndMessaging::WM_NULL,
            WPARAM(0),
            LPARAM(0),
        );
        if cmd.0 == 0 {
            None
        } else {
            Some(cmd.0 as u32)
        }
    }
}
