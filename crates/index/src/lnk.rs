//! Resolve .lnk shortcut targets via IShellLinkW (COM).
//!
//! The Start Menu scan treats each shortcut as its own app candidate, which
//! surfaces "Google Chrome" / "Chrome 无痕模式" / "卸载 Chrome" as separate
//! rows with shortcut-overlay icons. Resolving the real target lets the index
//! merge rows by exe and point icons at the real application.

use std::path::Path;

#[cfg(windows)]
use windows::core::{Interface, PCWSTR};
#[cfg(windows)]
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER,
    COINIT_MULTITHREADED, STGM_READ,
};
#[cfg(windows)]
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink, SLGP_UNCPRIORITY};

/// Everything worth keeping from a parsed shortcut.
#[derive(Debug, Clone, Default)]
pub struct LnkInfo {
    /// Real target path (exe / doc), if resolvable.
    pub target: Option<String>,
    /// Command-line arguments (empty when the shortcut has none).
    pub args: Option<String>,
    /// Custom icon location (path + icon index) when the shortcut overrides it.
    pub icon: Option<(String, i32)>,
}

/// Resolve a `.lnk` to its real target / args / icon.
///
/// Returns `None` on any failure (bad file, COM unavailable, broken link) —
/// the caller keeps the raw shortcut as a fallback row.
pub fn resolve_lnk(path: &Path) -> Option<LnkInfo> {
    #[cfg(windows)]
    {
        resolve_lnk_windows(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

#[cfg(windows)]
fn resolve_lnk_windows(path: &Path) -> Option<LnkInfo> {
    let path_str = path.to_string_lossy();
    with_com(|| {
        unsafe {
            // CoCreateInstance<_, IShellLinkW> infers the interface from the
            // return type; None for punkOuter (not aggregated).
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
            let persist: IPersistFile = link.cast().ok()?;

            let wide: Vec<u16> = path_str.encode_utf16().chain(std::iter::once(0)).collect();
            persist.Load(PCWSTR(wide.as_ptr()), STGM_READ).ok()?;

            let mut target_buf = vec![0u16; 1024];
            // pfd may be null (no need for WIN32_FIND_DATAW).
            link.GetPath(
                &mut target_buf,
                std::ptr::null_mut(),
                SLGP_UNCPRIORITY.0 as u32,
            )
            .ok()?;
            let target = utf16_trim(&target_buf);
            if target.is_empty() {
                return None;
            }

            let mut args_buf = vec![0u16; 1024];
            let args = link.GetArguments(&mut args_buf).ok().and_then(|_| {
                let s = utf16_trim(&args_buf);
                (!s.is_empty()).then_some(s)
            });

            let mut icon_buf = vec![0u16; 1024];
            let mut icon_index = 0i32;
            let icon = link
                .GetIconLocation(&mut icon_buf, &mut icon_index)
                .ok()
                .and_then(|_| {
                    let s = utf16_trim(&icon_buf);
                    (!s.is_empty()).then_some((s, icon_index))
                });

            Some(LnkInfo {
                target: Some(target),
                args,
                icon,
            })
        }
    })
}

/// Run `f` on a thread with COM initialized (multithreaded apartment).
///
/// CoInitializeEx may be called repeatedly on the same thread; only the call
/// that actually initializes the apartment must be paired with CoUninitialize.
#[cfg(windows)]
fn with_com<T>(f: impl FnOnce() -> Option<T>) -> Option<T> {
    const RPC_E_CHANGED_MODE: i32 = 0x80010106u32 as i32; // thread already init'd, other mode
    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let must_uninit = hr.is_ok(); // S_OK (fresh) or S_FALSE (already init'd)
    if !must_uninit && hr.0 != RPC_E_CHANGED_MODE {
        return None;
    }
    let result = f();
    if must_uninit {
        unsafe { CoUninitialize() };
    }
    result
}

/// Decode a UTF-16 buffer up to the first NUL.
#[cfg(windows)]
fn utf16_trim(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_returns_none() {
        // Should not panic; on non-Windows this is a no-op None.
        assert!(resolve_lnk(Path::new(r"C:\definitely\missing.lnk")).is_none());
    }
}
