//! Length-prefixed JSON-RPC 帧（Host ↔ Native 插件传输层）。
//!
//! 每帧 = 4 字节小端 uint32 长度（**不含**自身 4 字节）+ UTF-8 JSON body。
//! 独立于 Host ↔ UI 的 NDJSON：native 插件 stdout 必须纯净，任何杂质都会破坏解析。
//!
//! 长度上限 16 MiB，防止恶意/失控插件用大长度前缀让 host 立即 OOM。

use crate::IpcError;
use std::io::{Read, Write};

/// 单帧 body 最大字节数（16 MiB）。
pub const MAX_FRAME_LEN: u32 = 16 * 1024 * 1024;

/// 写一帧：4 字节小端长度 + body。
pub fn write_frame<W: Write>(w: &mut W, body: &[u8]) -> Result<(), IpcError> {
    let len = u32::try_from(body.len())
        .map_err(|_| IpcError::Invalid(format!("frame body too large: {} bytes", body.len())))?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(body)?;
    w.flush()?;
    Ok(())
}

/// 读一帧：先读 4 字节小端长度，再读对应字节数的 body。
///
/// 返回原始 body 字节；调用方负责反序列化。遇到 EOF（对端关闭且尚未读到长度字节）
/// 返回 `Ok(None)`，便于上层把“干净关闭”与“中途断开”区分开。
pub fn read_frame<R: Read>(r: &mut R) -> Result<Option<Vec<u8>>, IpcError> {
    let mut header = [0u8; 4];
    if !read_exact_or_eof(r, &mut header)? {
        // 对端在帧开始前就关闭：干净 EOF。
        return Ok(None);
    }
    let len = u32::from_le_bytes(header);
    if len == 0 {
        return Ok(Some(Vec::new()));
    }
    if len > MAX_FRAME_LEN {
        return Err(IpcError::Invalid(format!(
            "frame length {len} exceeds limit {MAX_FRAME_LEN}"
        )));
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body)?;
    Ok(Some(body))
}

/// 读满 buf；返回 false 表示在还没读到任何字节时就 EOF（干净关闭），
/// true 表示已读满。读到一半 EOF 会按 read_exact 的语义报 UnexpectedEof。
fn read_exact_or_eof<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<bool, IpcError> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = r.read(&mut buf[filled..])?;
        if n == 0 {
            if filled == 0 {
                return Ok(false);
            }
            // 已经读到部分长度字节后 EOF：半帧 = 协议错误，而非干净关闭。
            return Err(IpcError::Invalid(
                "unexpected eof mid-frame header".to_string(),
            ));
        }
        filled += n;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip() {
        let body = br#"{"jsonrpc":"2.0","id":1,"method":"plugin.query","params":{}}"#;
        let mut out = Vec::new();
        write_frame(&mut out, body).unwrap();
        // 4 字节小端长度 + body
        assert_eq!(
            u32::from_le_bytes(out[..4].try_into().unwrap()) as usize,
            body.len()
        );
        let mut cur = std::io::Cursor::new(out);
        let got = read_frame(&mut cur).unwrap().unwrap();
        assert_eq!(got, body);
    }

    #[test]
    fn empty_body_round_trip() {
        let mut out = Vec::new();
        write_frame(&mut out, &[]).unwrap();
        let mut cur = std::io::Cursor::new(out);
        let got = read_frame(&mut cur).unwrap().unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn clean_eof_returns_none() {
        let mut cur = std::io::Cursor::new(Vec::new());
        assert!(read_frame(&mut cur).unwrap().is_none());
    }

    #[test]
    fn oversized_frame_rejected() {
        // 伪造一个超长长度前缀，但不实际传 body。
        let mut bad = Vec::new();
        bad.extend_from_slice(&(MAX_FRAME_LEN + 1).to_le_bytes());
        let mut cur = std::io::Cursor::new(bad);
        assert!(read_frame(&mut cur).is_err());
    }

    #[test]
    fn mid_frame_eof_is_error() {
        // 只给 2 字节 header 后 EOF。
        let mut cur = std::io::Cursor::new(vec![0u8, 0u8]);
        assert!(read_frame(&mut cur).is_err());
    }
}
