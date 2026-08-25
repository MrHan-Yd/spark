//! `signature.json` 读取、文件哈希重算、Ed25519 验签（规范 §4–§6）。
//!
//! 返回契约（规范 §6.2）：
//! - `Ok(Unsigned)`：插件目录无 `signature.json`。
//! - `Ok(Invalid)`：有 `signature.json` 但验签失败——哈希不匹配 / 签名不过 / 格式错 /
//!   id·version 不匹配 / key_id 未命中可信表 / 命中吊销列表。**不作为 Err 抛出**，
//!   便于 `list()`/`scan_standard` 把失效状态展示给 UI；install 侧据策略转 `Err`。
//! - `Ok(Official)`/`Ok(ThirdParty)`：验过，按命中密钥的 `KeyKind` 决定。
//! - `Err(VerifyError)`：仅真实的文件系统遍历失败（目录不可读等）。
//!
//! 验签公钥**恒来自可信表**（内置 `TRUSTED_KEYS`，或 PluginManager 合并了
//! 用户导入三方密钥的运行时表；sign-tool 可传单公钥表），
//! `signature.json` 里的 `key_id` 仅作索引，不被信任为公钥来源。
//!
//! 3.2 起**全量重验**：重算目录内每个文件的哈希与 `signature.json` 清单双向比对，
//! 安装与启动扫描（scan_standard）同走此路径，无轻量分支。

use crate::signing::canon::{canonical_bytes, collect_file_entries, FileEntry};
use crate::signing::{is_revoked, Revocation, TrustedKey};
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use thiserror::Error;

/// 插件签名状态。序列化形状即 UI/IPC DTO（`snake_case`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignState {
    /// 签名存在且验过，且 key_id 是官方密钥。
    Official,
    /// 签名存在且验过，但 key_id 是用户导入的三方密钥（v1 不产生）。
    ThirdParty,
    /// 无 `signature.json`。
    Unsigned,
    /// 有 `signature.json` 但验签失败（install 时已拒；scan 重验时展示给 UI）。
    Invalid,
}

/// 验签过程的不可恢复错误（仅文件系统层面）。签名内容问题一律映射为 `Ok(Invalid)`。
#[derive(Debug, Error)]
pub enum VerifyError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// `signature.json` 反序列化结构（规范 §4.2）。
#[derive(Debug, Deserialize)]
struct SignatureFile {
    schema: u32,
    plugin_id: String,
    version: String,
    algorithm: String,
    key_id: String,
    #[serde(default)]
    _signed_at: Option<String>,
    files: Vec<FileEntry>,
    signature: String,
}

/// 用默认表（内置官方公钥 + 内置吊销列表）验签。install/scan 路径由
/// `PluginManager` 走 `verify_with_keys`（传入合并了用户三方密钥的运行时表）。
pub fn verify_dir(
    dir: &Path,
    expected_id: &str,
    expected_version: &str,
) -> Result<SignState, VerifyError> {
    verify_with_keys(
        dir,
        expected_id,
        expected_version,
        crate::signing::TRUSTED_KEYS,
        crate::signing::REVOKED,
    )
}

/// 用调用方提供的可信公钥表与吊销列表验签（全量重算）。
///
/// - host：`PluginManager` 内置表 + 用户导入三方密钥的合并表（规范 §5.3 / §10）。
/// - CI/开发者自检：`spark-sign verify --pubkey`。
pub fn verify_with_keys(
    dir: &Path,
    expected_id: &str,
    expected_version: &str,
    keys: &[TrustedKey],
    revoked: &[Revocation],
) -> Result<SignState, VerifyError> {
    let sig_path = dir.join("signature.json");
    if !sig_path.is_file() {
        return Ok(SignState::Unsigned);
    }

    // 读取并解析 signature.json：格式错一律视作 Invalid（可能被篡改/损坏）。
    let raw = match fs::read_to_string(&sig_path) {
        Ok(s) => s,
        Err(_) => return Ok(SignState::Invalid),
    };
    let sig: SignatureFile = match serde_json::from_str(&raw) {
        Ok(s) => s,
        Err(_) => return Ok(SignState::Invalid),
    };

    // schema / algorithm 校验：前向兼容靠 bump schema，当前只认 schema=1 + ed25519。
    if sig.schema != 1 || sig.algorithm != "ed25519" {
        return Ok(SignState::Invalid);
    }
    // plugin_id / version 必须与 plugin.json（调用方传入）一致。
    if sig.plugin_id != expected_id || sig.version != expected_version {
        return Ok(SignState::Invalid);
    }
    // 路径规范化校验 + 去重 + sha256 格式校验。
    let mut seen: HashSet<&str> = HashSet::new();
    for f in &sig.files {
        if !is_canonical_rel_path(&f.path) || !seen.insert(f.path.as_str()) {
            return Ok(SignState::Invalid);
        }
        if !is_lower_hex_64(&f.sha256) {
            return Ok(SignState::Invalid);
        }
    }

    // 全量双向校验 + 哈希重算：复用 canon 的 collect_file_entries（与签名端同源），
    // 一次遍历拿到磁盘上除 signature.json 外全部普通文件的 (path, sha256)。
    // 避免签名端/验签端两份独立的目录遍历实现产生漂移。文件系统遍历失败属
    // 真实 io 错误，按规范 §6.2 抛 Err。
    // plugin.json 必须列在 files[] 中且哈希一致（授权文档未覆盖等同未签名）。
    let disk = collect_file_entries(dir)?;
    if disk.len() != sig.files.len() {
        return Ok(SignState::Invalid);
    }
    let mut disk_by_path: std::collections::HashMap<&str, &str> = disk
        .iter()
        .map(|e| (e.path.as_str(), e.sha256.as_str()))
        .collect();
    for f in &sig.files {
        match disk_by_path.remove(f.path.as_str()) {
            // 磁盘上无此路径（悬空）或哈希不匹配 → Invalid。
            Some(actual_hash) if actual_hash == f.sha256.as_str() => {}
            _ => return Ok(SignState::Invalid),
        }
    }
    // disk_by_path 此时非空表示磁盘上有 files[] 未列出的文件（遗漏）→ Invalid。
    if !disk_by_path.is_empty() {
        return Ok(SignState::Invalid);
    }

    // 找到 key_id 对应的可信公钥；未命中视为不可验证（key_id 仅索引，公钥恒来自可信表）。
    let key = match keys.iter().find(|k| k.key_id.as_ref() == sig.key_id) {
        Some(k) => k,
        None => return Ok(SignState::Invalid),
    };
    if key.algorithm.as_ref() != "ed25519" {
        return Ok(SignState::Invalid);
    }

    // 解码公钥与签名，重建 canonical bytes 后 Ed25519 verify。
    let pubkey_bytes = match base64_decode(key.public_key.as_ref()) {
        Ok(b) => b,
        Err(_) => return Ok(SignState::Invalid),
    };
    let pubkey_arr: [u8; 32] = match pubkey_bytes.try_into() {
        Ok(a) => a,
        Err(_) => return Ok(SignState::Invalid),
    };
    let verifying_key = match VerifyingKey::from_bytes(&pubkey_arr) {
        Ok(v) => v,
        Err(_) => return Ok(SignState::Invalid),
    };
    let sig_bytes = match base64_decode(&sig.signature) {
        Ok(b) => b,
        Err(_) => return Ok(SignState::Invalid),
    };
    let signature = match Signature::from_slice(&sig_bytes) {
        Ok(s) => s,
        Err(_) => return Ok(SignState::Invalid),
    };

    // canonical bytes 用 signature.json 自述的 id/version（已校验与 plugin.json 一致）。
    let canon = canonical_bytes(
        &sig.plugin_id,
        &sig.version,
        &sig.algorithm,
        &sig.key_id,
        &sig.files,
    );
    if verifying_key.verify(&canon, &signature).is_err() {
        return Ok(SignState::Invalid);
    }

    // 验过后再查吊销列表：命中即 Invalid（规范 §8.2）。
    if is_revoked(revoked, key.key_id.as_ref(), expected_id, expected_version) {
        return Ok(SignState::Invalid);
    }

    Ok(match key.kind {
        crate::signing::KeyKind::Official => SignState::Official,
        crate::signing::KeyKind::ThirdParty => SignState::ThirdParty,
    })
}

/// POSIX 相对路径规范性校验（规范 §4.3）：
/// 非空、无前导 `/`、无反斜杠、无 `\0`、无盘符冒号、无 `.`/`..`/空段。
fn is_canonical_rel_path(p: &str) -> bool {
    if p.is_empty() || p.starts_with('/') {
        return false;
    }
    if p.contains('\\') || p.contains('\0') || p.contains(':') {
        return false;
    }
    for seg in p.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return false;
        }
    }
    true
}

/// 64 位小写十六进制校验。
fn is_lower_hex_64(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn base64_decode(s: &str) -> Result<Vec<u8>, base64::DecodeError> {
    base64::engine::general_purpose::STANDARD.decode(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signing::KeyKind;
    use ed25519_dalek::{Signer, SigningKey};
    use std::borrow::Cow;
    use std::fs;
    use std::path::PathBuf;

    /// 用固定 32 字节 seed 构造签名密钥（确定性，无需 RNG 依赖）。
    fn signing_key_from_seed(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    /// 构造一个带签名目录的测试插件：写 plugin.json + 给定文件 + signature.json。
    /// `files` 为 `(相对路径, 内容)` 列表。
    fn make_signed_plugin(
        dir: &Path,
        id: &str,
        version: &str,
        key_id: &str,
        signing_key: &SigningKey,
        files: &[(&str, &str)],
    ) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        // plugin.json
        let plugin_json = format!(
            r#"{{"id":"{id}","name":"T","version":"{version}","api_version":2,"runtime":"webview","main":"index.html","features":[{{"type":"keyword","keyword":"t","title":"T","mode":"page"}}]}}"#
        );
        fs::write(dir.join("plugin.json"), plugin_json).unwrap();
        // 其余文件
        for (p, content) in files {
            let full = dir.join(p);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(full, content).unwrap();
        }
        // 算每个文件（含 plugin.json）的 sha256，建 FileEntry 清单。
        // 复用 canon 的 collect_file_entries，与签名端/验签端同源。
        let entries: Vec<FileEntry> = collect_file_entries(dir).unwrap();
        // signature.json 自身不列入 files（规范 §4.2）。
        let canon = canonical_bytes(id, version, "ed25519", key_id, &entries);
        let signature = signing_key.sign(&canon);
        let sig_b64 = base64::engine::general_purpose::STANDARD.encode(signature.to_bytes());
        // files[] 序列化
        let files_json: Vec<serde_json::Value> = entries
            .iter()
            .map(|e| serde_json::json!({"path": e.path, "sha256": e.sha256}))
            .collect();
        let sig_json = serde_json::json!({
            "schema": 1,
            "plugin_id": id,
            "version": version,
            "algorithm": "ed25519",
            "key_id": key_id,
            "signed_at": "2026-08-25T10:00:00Z",
            "files": files_json,
            "signature": sig_b64,
        });
        fs::write(
            dir.join("signature.json"),
            serde_json::to_string_pretty(&sig_json).unwrap(),
        )
        .unwrap();
        dir.to_path_buf()
    }

    fn trusted_key_for(key_id: &str, signing_key: &SigningKey, kind: KeyKind) -> TrustedKey {
        let vk = signing_key.verifying_key();
        let pubkey_b64 = base64::engine::general_purpose::STANDARD.encode(vk.to_bytes());
        TrustedKey {
            key_id: Cow::Owned(key_id.to_string()),
            algorithm: Cow::Borrowed("ed25519"),
            public_key: Cow::Owned(pubkey_b64),
            kind,
            note: Cow::Borrowed("test-only key"),
        }
    }

    #[test]
    fn official_signed_plugin_verifies() {
        let tmp = std::env::temp_dir().join("spark_sig_official");
        let _ = fs::remove_dir_all(&tmp);
        let sk = signing_key_from_seed(7);
        let key = trusted_key_for("spark-test-v1", &sk, KeyKind::Official);
        make_signed_plugin(
            &tmp,
            "com.spark.translate",
            "0.2.0",
            "spark-test-v1",
            &sk,
            &[
                ("index.html", "<html></html>"),
                ("assets/style.css", "body{}"),
            ],
        );
        let state = verify_with_keys(&tmp, "com.spark.translate", "0.2.0", &[key], &[]).unwrap();
        assert_eq!(state, SignState::Official);
    }

    #[test]
    fn third_party_key_verifies_as_third_party() {
        let tmp = std::env::temp_dir().join("spark_sig_thirdparty");
        let _ = fs::remove_dir_all(&tmp);
        let sk = signing_key_from_seed(9);
        let key = trusted_key_for("dev-v1", &sk, KeyKind::ThirdParty);
        make_signed_plugin(
            &tmp,
            "com.dev.tool",
            "1.0.0",
            "dev-v1",
            &sk,
            &[("index.html", "<html></html>")],
        );
        let state = verify_with_keys(&tmp, "com.dev.tool", "1.0.0", &[key], &[]).unwrap();
        assert_eq!(state, SignState::ThirdParty);
    }

    #[test]
    fn tampered_file_becomes_invalid() {
        let tmp = std::env::temp_dir().join("spark_sig_tamper");
        let _ = fs::remove_dir_all(&tmp);
        let sk = signing_key_from_seed(7);
        let key = trusted_key_for("spark-test-v1", &sk, KeyKind::Official);
        make_signed_plugin(
            &tmp,
            "com.spark.translate",
            "0.2.0",
            "spark-test-v1",
            &sk,
            &[("index.html", "<html>original</html>")],
        );
        // 篡改文件内容。
        fs::write(tmp.join("index.html"), "<html>hacked</html>").unwrap();
        let state = verify_with_keys(&tmp, "com.spark.translate", "0.2.0", &[key], &[]).unwrap();
        assert_eq!(state, SignState::Invalid);
    }

    #[test]
    fn no_signature_json_is_unsigned() {
        let tmp = std::env::temp_dir().join("spark_sig_unsigned");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::write(
            tmp.join("plugin.json"),
            r#"{"id":"com.spark.x","name":"X","version":"0.1.0","api_version":2,"runtime":"webview","main":"index.html","features":[{"type":"keyword","keyword":"x","title":"X","mode":"page"}]}"#,
        )
        .unwrap();
        fs::write(tmp.join("index.html"), "<html></html>").unwrap();
        let state = verify_with_keys(&tmp, "com.spark.x", "0.1.0", &[], &[]).unwrap();
        assert_eq!(state, SignState::Unsigned);
    }

    #[test]
    fn id_mismatch_is_invalid() {
        let tmp = std::env::temp_dir().join("spark_sig_idmismatch");
        let _ = fs::remove_dir_all(&tmp);
        let sk = signing_key_from_seed(7);
        let key = trusted_key_for("spark-test-v1", &sk, KeyKind::Official);
        make_signed_plugin(
            &tmp,
            "com.spark.translate",
            "0.2.0",
            "spark-test-v1",
            &sk,
            &[("index.html", "<html></html>")],
        );
        // 期望 id 与签名里的不一致。
        let state = verify_with_keys(&tmp, "com.spark.other", "0.2.0", &[key], &[]).unwrap();
        assert_eq!(state, SignState::Invalid);
    }

    #[test]
    fn unknown_key_id_is_invalid() {
        let tmp = std::env::temp_dir().join("spark_sig_unknown_key");
        let _ = fs::remove_dir_all(&tmp);
        let sk = signing_key_from_seed(7);
        // 可信表里没有该 key_id。
        make_signed_plugin(
            &tmp,
            "com.spark.translate",
            "0.2.0",
            "spark-test-v1",
            &sk,
            &[("index.html", "<html></html>")],
        );
        let state = verify_with_keys(&tmp, "com.spark.translate", "0.2.0", &[], &[]).unwrap();
        assert_eq!(state, SignState::Invalid);
    }

    #[test]
    fn corrupted_signature_is_invalid() {
        let tmp = std::env::temp_dir().join("spark_sig_corrupt");
        let _ = fs::remove_dir_all(&tmp);
        let sk = signing_key_from_seed(7);
        let key = trusted_key_for("spark-test-v1", &sk, KeyKind::Official);
        make_signed_plugin(
            &tmp,
            "com.spark.translate",
            "0.2.0",
            "spark-test-v1",
            &sk,
            &[("index.html", "<html></html>")],
        );
        // 改坏 signature.json 的 signature 字段。
        let raw = fs::read_to_string(tmp.join("signature.json")).unwrap();
        let mut val: serde_json::Value = serde_json::from_str(&raw).unwrap();
        val["signature"] = serde_json::json!("AAAAA".repeat(12));
        fs::write(
            tmp.join("signature.json"),
            serde_json::to_string(&val).unwrap(),
        )
        .unwrap();
        let state = verify_with_keys(&tmp, "com.spark.translate", "0.2.0", &[key], &[]).unwrap();
        assert_eq!(state, SignState::Invalid);
    }

    #[test]
    fn extra_file_on_disk_is_invalid() {
        let tmp = std::env::temp_dir().join("spark_sig_extra");
        let _ = fs::remove_dir_all(&tmp);
        let sk = signing_key_from_seed(7);
        let key = trusted_key_for("spark-test-v1", &sk, KeyKind::Official);
        make_signed_plugin(
            &tmp,
            "com.spark.translate",
            "0.2.0",
            "spark-test-v1",
            &sk,
            &[("index.html", "<html></html>")],
        );
        // 磁盘多一个文件但未列入 files[]。
        fs::write(tmp.join("sneak.txt"), "injected").unwrap();
        let state = verify_with_keys(&tmp, "com.spark.translate", "0.2.0", &[key], &[]).unwrap();
        assert_eq!(state, SignState::Invalid);
    }

    #[test]
    fn dangling_file_in_manifest_is_invalid() {
        let tmp = std::env::temp_dir().join("spark_sig_dangling");
        let _ = fs::remove_dir_all(&tmp);
        let sk = signing_key_from_seed(7);
        let key = trusted_key_for("spark-test-v1", &sk, KeyKind::Official);
        // 正常签名（只含 index.html）。
        make_signed_plugin(
            &tmp,
            "com.spark.translate",
            "0.2.0",
            "spark-test-v1",
            &sk,
            &[("index.html", "<html></html>")],
        );
        // 往 signature.json 的 files[] 塞一个磁盘上不存在的条目，并重签名。
        let mut val: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(tmp.join("signature.json")).unwrap()).unwrap();
        val["files"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "path": "ghost.txt",
                "sha256": "0000000000000000000000000000000000000000000000000000000000000000"
            }));
        // 重算 canonical bytes 并重签（用同一 key）。
        let entries: Vec<FileEntry> = val["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| serde_json::from_value(e.clone()).unwrap())
            .collect();
        let canon = canonical_bytes(
            "com.spark.translate",
            "0.2.0",
            "ed25519",
            "spark-test-v1",
            &entries,
        );
        let sig = sk.sign(&canon);
        val["signature"] =
            serde_json::json!(base64::engine::general_purpose::STANDARD.encode(sig.to_bytes()));
        fs::write(
            tmp.join("signature.json"),
            serde_json::to_string(&val).unwrap(),
        )
        .unwrap();
        let state = verify_with_keys(&tmp, "com.spark.translate", "0.2.0", &[key], &[]).unwrap();
        assert_eq!(state, SignState::Invalid);
    }

    #[test]
    fn revoked_key_is_invalid() {
        let tmp = std::env::temp_dir().join("spark_sig_revoked");
        let _ = fs::remove_dir_all(&tmp);
        let sk = signing_key_from_seed(7);
        let key = trusted_key_for("spark-test-v1", &sk, KeyKind::Official);
        make_signed_plugin(
            &tmp,
            "com.spark.translate",
            "0.2.0",
            "spark-test-v1",
            &sk,
            &[("index.html", "<html></html>")],
        );
        let revoked = &[Revocation {
            key_id: "spark-test-v1",
            plugin_id: None,
            version: None,
            reason: "test revocation",
        }];
        let state =
            verify_with_keys(&tmp, "com.spark.translate", "0.2.0", &[key], revoked).unwrap();
        assert_eq!(state, SignState::Invalid);
    }

    #[test]
    fn forged_official_key_id_is_invalid() {
        // 用一把与内置官方公钥不匹配的私钥签名，但 key_id 填 spark-official-v1：
        // host 用内置官方公钥验这把伪造签名 → 验不过 → Invalid。
        // 这验证了"key_id 仅索引、公钥恒来自内置表"的核心不变量（规范 §4.4）。
        let tmp = std::env::temp_dir().join("spark_sig_forged_official");
        let _ = fs::remove_dir_all(&tmp);
        let sk = signing_key_from_seed(7);
        make_signed_plugin(
            &tmp,
            "com.spark.translate",
            "0.2.0",
            "spark-official-v1",
            &sk,
            &[("index.html", "<html></html>")],
        );
        let state = verify_dir(&tmp, "com.spark.translate", "0.2.0").unwrap();
        assert_eq!(state, SignState::Invalid);
    }

    #[test]
    fn canonical_rel_path_validation() {
        assert!(is_canonical_rel_path("plugin.json"));
        assert!(is_canonical_rel_path("assets/style.css"));
        assert!(is_canonical_rel_path("a/b/c.txt"));
        // 非法
        assert!(!is_canonical_rel_path(""));
        assert!(!is_canonical_rel_path("/abs"));
        assert!(!is_canonical_rel_path("a\\b"));
        assert!(!is_canonical_rel_path("./a"));
        assert!(!is_canonical_rel_path("a/./b"));
        assert!(!is_canonical_rel_path("../a"));
        assert!(!is_canonical_rel_path("a/../b"));
        assert!(!is_canonical_rel_path("a//b"));
        assert!(!is_canonical_rel_path("a/"));
        assert!(!is_canonical_rel_path("C:/x"));
    }

    #[test]
    fn lower_hex_64_validation() {
        assert!(is_lower_hex_64(
            "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
        ));
        assert!(!is_lower_hex_64("ABCDEF")); // 过短
        assert!(!is_lower_hex_64(&"A".repeat(64))); // 大写
        assert!(!is_lower_hex_64(&"g".repeat(64))); // 非十六进制
    }
}
