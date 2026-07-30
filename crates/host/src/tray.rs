//! System notification-area icon (Shell_NotifyIconW).

use anyhow::Result;
use std::mem;
use std::path::Path;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
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

    pub fn remove(&mut self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.data);
        }
    }
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
