//! Global hotkey registration (RegisterHotKey). Default Alt+Space.

use anyhow::{bail, Context, Result};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN,
};
use windows::Win32::UI::WindowsAndMessaging::WM_USER;

/// Custom tray / host messages start here.
pub const WM_SPARK_TRAY: u32 = WM_USER + 1;
pub const HOTKEY_ID_TOGGLE: i32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hotkey {
    pub modifiers: HOT_KEY_MODIFIERS,
    pub vk: u32,
}

impl Hotkey {
    pub fn parse(s: &str) -> Result<Self> {
        let mut modifiers = HOT_KEY_MODIFIERS(0);
        let mut key: Option<u32> = None;
        for part in s.split('+').map(|p| p.trim()) {
            if part.is_empty() {
                continue;
            }
            let lower = part.to_ascii_lowercase();
            match lower.as_str() {
                "alt" | "menu" => modifiers |= MOD_ALT,
                "ctrl" | "control" => modifiers |= MOD_CONTROL,
                "shift" => modifiers |= MOD_SHIFT,
                "win" | "super" | "meta" => modifiers |= MOD_WIN,
                "space" | "spc" => key = Some(0x20), // VK_SPACE
                "tab" => key = Some(0x09),
                "esc" | "escape" => key = Some(0x1B),
                other if other.len() == 1 => {
                    let c = other.chars().next().unwrap().to_ascii_uppercase();
                    if c.is_ascii_alphanumeric() {
                        key = Some(c as u32);
                    } else {
                        bail!("unsupported key: {part}");
                    }
                }
                other if other.starts_with('f') && other.len() <= 3 => {
                    if let Ok(n) = other[1..].parse::<u32>() {
                        if (1..=24).contains(&n) {
                            key = Some(0x70 + (n - 1)); // VK_F1
                            continue;
                        }
                    }
                    bail!("unsupported key: {part}");
                }
                _ => bail!("unsupported hotkey token: {part}"),
            }
        }
        let vk = key.context("hotkey missing key (e.g. Space)")?;
        if modifiers.0 == 0 {
            bail!("hotkey must include a modifier (Alt/Ctrl/Shift/Win)");
        }
        Ok(Self { modifiers, vk })
    }

    pub fn register(self, hwnd: HWND, id: i32) -> Result<()> {
        unsafe { RegisterHotKey(Some(hwnd), id, self.modifiers, self.vk) }
            .context("RegisterHotKey failed (conflict?). Try another combination in config.")?;
        Ok(())
    }

    pub fn unregister(hwnd: HWND, id: i32) {
        unsafe {
            let _ = UnregisterHotKey(Some(hwnd), id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_alt_space() {
        let h = Hotkey::parse("Alt+Space").unwrap();
        assert_eq!(h.vk, 0x20);
        assert!(h.modifiers.0 & MOD_ALT.0 != 0);
    }

    #[test]
    fn parse_ctrl_space() {
        let h = Hotkey::parse("Ctrl+Space").unwrap();
        assert!(h.modifiers.0 & MOD_CONTROL.0 != 0);
    }
}
