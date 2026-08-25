//! Canonical bytes 拼装（规范 §4.4）。
//!
//! 签名工具与 host 验签**共用本函数**，保证两端对同一份 `signature.json` 算出的
//! canonical bytes 逐字节一致。所有 `\n` 为单 LF、无 BOM；路径按 **UTF-8 字节序**
//! 排序（中文文件名两端一致）；`sha256` 为 64 位小写十六进制。

use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::fs;
use std::io::Read;
use std::path::Path;

/// `signature.json` 里 `files[]` 的一条记录：文件相对路径 + 内容 SHA-256。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileEntry {
    /// POSIX 风格相对路径（`/`），不含前导 `/`、不含 `..`、不含反斜杠。
    pub path: String,
    /// 64 位小写十六进制 SHA-256。
    pub sha256: String,
}

/// 按 §4.4 规则拼装被签名的 canonical bytes。
///
/// 格式：
/// ```text
/// spark-plugin-signature-v1\n
/// {plugin_id}\n
/// {version}\n
/// {algorithm}\n
/// {key_id}\n
/// {sha256}  {path}\n   （按 path 的 UTF-8 字节序，行尾 LF）
/// ```
///
/// `files` 按 `path` 的 **UTF-8 字节序** 排序后参与拼装，原切片顺序不影响结果。
/// 双空格分隔 sha256 与 path，与 `sha256sum` 文本格式一致。
pub fn canonical_bytes(
    plugin_id: &str,
    version: &str,
    algorithm: &str,
    key_id: &str,
    files: &[FileEntry],
) -> Vec<u8> {
    let mut sorted: Vec<&FileEntry> = files.iter().collect();
    // 按 path 的 UTF-8 字节序排序（非 Unicode 码点序），锁死两端一致。
    sorted.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));

    let mut out = String::new();
    out.push_str("spark-plugin-signature-v1\n");
    out.push_str(plugin_id);
    out.push('\n');
    out.push_str(version);
    out.push('\n');
    out.push_str(algorithm);
    out.push('\n');
    out.push_str(key_id);
    out.push('\n');
    for f in sorted {
        out.push_str(&f.sha256);
        out.push_str("  ");
        out.push_str(&f.path);
        out.push('\n');
    }
    out.into_bytes()
}

/// 递归收集 `dir` 下除 `signature.json` 外的全部普通文件，算每个文件 SHA-256，
/// 返回按 path 的 **UTF-8 字节序** 排序后的 `FileEntry` 清单。
///
/// sign-tool 的 `sign` 命令用本函数构建 `files[]`（与 host 验签侧的收集规则同源，
/// 避免两端漂移）。跳过符号链接（与 §4.3 / Zip Slip 防护语义一致）。
pub fn collect_file_entries(dir: &Path) -> std::io::Result<Vec<FileEntry>> {
    let mut paths: Vec<String> = Vec::new();
    walk_collect(dir, dir, &mut paths)?;
    // 按 path 的 UTF-8 字节序排序，与 canonical_bytes 的排序一致。
    paths.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    paths
        .into_iter()
        .map(|p| {
            let sha256 = sha256_file(&dir.join(&p))?;
            Ok(FileEntry { path: p, sha256 })
        })
        .collect()
}

fn walk_collect(root: &Path, cur: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
    for entry in fs::read_dir(cur)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            continue;
        }
        let path = entry.path();
        if ft.is_dir() {
            walk_collect(root, &path, out)?;
        } else if ft.is_file() {
            let rel = path.strip_prefix(root).unwrap();
            let posix = rel.to_string_lossy().replace('\\', "/");
            if posix == "signature.json" {
                continue;
            }
            out.push(posix);
        }
    }
    Ok(())
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = fs::File::open(path)?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_bytes_are_stable() {
        // 固定输入 → 固定字节。任何改动都会让 golden test 失败，锁死格式。
        let files = vec![
            FileEntry {
                path: "assets/style.css".into(),
                sha256: "aaaa".into(),
            },
            FileEntry {
                path: "plugin.json".into(),
                sha256: "bbbb".into(),
            },
            FileEntry {
                path: "index.html".into(),
                sha256: "cccc".into(),
            },
        ];
        let bytes = canonical_bytes(
            "com.spark.translate",
            "0.2.0",
            "ed25519",
            "spark-official-v1",
            &files,
        );
        let golden = b"spark-plugin-signature-v1\n\
            com.spark.translate\n\
            0.2.0\n\
            ed25519\n\
            spark-official-v1\n\
            aaaa  assets/style.css\n\
            cccc  index.html\n\
            bbbb  plugin.json\n";
        assert_eq!(bytes, golden);
    }

    #[test]
    fn input_order_does_not_affect_output() {
        let a = vec![
            FileEntry {
                path: "a".into(),
                sha256: "1".into(),
            },
            FileEntry {
                path: "b".into(),
                sha256: "2".into(),
            },
        ];
        let b = vec![
            FileEntry {
                path: "b".into(),
                sha256: "2".into(),
            },
            FileEntry {
                path: "a".into(),
                sha256: "1".into(),
            },
        ];
        assert_eq!(
            canonical_bytes("id", "1.0.0", "ed25519", "k", &a),
            canonical_bytes("id", "1.0.0", "ed25519", "k", &b)
        );
    }

    #[test]
    fn sorts_by_utf8_byte_order() {
        // "é" (U+00E9) UTF-8 = 0xC3 0xA9；"日" (U+65E5) UTF-8 = 0xE6 0x97 0xA5。
        // 字节序：0xC3 < 0xE6 → "é.txt" 在 "日.txt" 前，与码点序一致此例不区分，
        // 用一对确保稳定即可。
        let files = vec![
            FileEntry {
                path: "日.txt".into(),
                sha256: "z".into(),
            },
            FileEntry {
                path: "é.txt".into(),
                sha256: "y".into(),
            },
        ];
        let bytes = canonical_bytes("id", "1", "ed25519", "k", &files);
        let s = std::str::from_utf8(&bytes).unwrap();
        // é 的首字节 0xC3 < 日 的首字节 0xE6，故 é 行在前。
        let é_pos = s.find("y  é.txt").unwrap();
        let 日_pos = s.find("z  日.txt").unwrap();
        assert!(é_pos < 日_pos);
    }
}
