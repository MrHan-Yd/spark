//! Win32 剪贴板读写（`spark.clipboard.*` 桥到 host 执行）。
//!
//! 仅 CF_UNICODETEXT 文本；一期够用。图片等后续增量。

use anyhow::{anyhow, Result};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows::Win32::System::Ole::CF_UNICODETEXT;

/// 读取剪贴板文本。空或非文本返回空串。
pub fn read_text() -> Result<String> {
    unsafe {
        OpenClipboard(None)?;
        let res = (|| -> Result<String> {
            let handle = match GetClipboardData(CF_UNICODETEXT.0 as u32) {
                Ok(h) => h,
                Err(_) => return Ok(String::new()),
            };
            // GetClipboardData 返回 HANDLE；CF_UNICODETEXT 的底层是 HGLOBAL，
            // 用相同裸指针构造 HGLOBAL 传给 GlobalLock/GlobalSize/GlobalUnlock。
            let hglobal = HGLOBAL(handle.0);
            let ptr = GlobalLock(hglobal) as *const u16;
            if ptr.is_null() {
                return Ok(String::new());
            }
            let size = GlobalSize(hglobal);
            let len = size / 2;
            let slice = std::slice::from_raw_parts(ptr, len);
            let s = String::from_utf16_lossy(slice);
            let _ = GlobalUnlock(hglobal);
            Ok(s.trim_end_matches('\0').to_string())
        })();
        let _ = CloseClipboard();
        res
    }
}

/// 写入剪贴板文本。
///
/// 资源管理：`GlobalAlloc` 分配的 `HGLOBAL` 在所有"未成功转移所有权给剪贴板"
/// 的路径上必须 `GlobalFree`；仅当 `SetClipboardData` 成功时所有权转移、不再释放。
pub fn write_text(text: &str) -> Result<()> {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0); // 结尾 NUL
    let bytes = wide.len() * 2;
    unsafe {
        let hglobal = GlobalAlloc(GMEM_MOVEABLE, bytes)?;
        // 从此处起 hglobal 需要回收，除非 SetClipboardData 成功转移所有权。

        let ptr = GlobalLock(hglobal) as *mut u16;
        if ptr.is_null() {
            let _ = GlobalFree(Some(hglobal));
            return Err(anyhow!("GlobalLock returned null"));
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
        let _ = GlobalUnlock(hglobal);

        if let Err(e) = OpenClipboard(None) {
            let _ = GlobalFree(Some(hglobal));
            return Err(e.into());
        }
        // clipboard 已打开：SetClipboardData 成功 → 所有权转移；失败 → 需回收。
        let transferred = (|| -> Result<()> {
            EmptyClipboard()?;
            let handle = HANDLE(hglobal.0);
            SetClipboardData(CF_UNICODETEXT.0 as u32, Some(handle))?;
            Ok(())
        })();
        let _ = CloseClipboard();
        if transferred.is_err() {
            let _ = GlobalFree(Some(hglobal));
        }
        transferred
    }
}
