//! spark-sign：Spark 插件签名工具（三期）。
//!
//! 独立于 host，只在签名机/CI 跑。规范见 `插件开发/插件签名规范.md` §9。
//!
//! 命令：
//! - `keygen`：生成 Ed25519 密钥对，写 base64 私钥文件 + 打印 base64 公钥
//! - `sign`：读插件目录，算文件哈希、拼 canonical bytes、签名、写 `signature.json`
//! - `verify`：重算哈希 + 验签（用内置公钥表，或 `--pubkey` 指定）
//! - `inspect`：打印 `signature.json` 与验签结果
//! - `check-registry`：官方仓库准入校验（3.1 强制签名，规范 §12.2）
//!
//! canonicalization 与文件清单收集**复用 `spark-plugin-manager::signing`**，保证
//! 签名端与 host 验签端字节一致（规范 §9.1）。

use anyhow::{bail, Context, Result};
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use spark_plugin_manager::{
    canonical_bytes, collect_file_entries, verify_dir, verify_with_keys, FileEntry, KeyKind,
    PluginManifest, SignState, TrustedKey, TRUSTED_KEYS,
};
use std::fs;
use std::io::Write;
use std::path::Path;

/// 私钥文件格式：单行 base64 的 32 字节 Ed25519 seed（规范 §9.1）。
/// v1 明文存储 + 文件权限保护；生产应加密或放 CI secret。
pub fn write_private_key(path: &Path, sk: &SigningKey) -> Result<()> {
    let b64 = base64::engine::general_purpose::STANDARD.encode(sk.to_bytes());
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(b64.as_bytes())?;
    Ok(())
}

/// 读取私钥文件（base64 seed → SigningKey）。
pub fn read_private_key(path: &Path) -> Result<SigningKey> {
    let raw = fs::read_to_string(path).with_context(|| format!("read key {}", path.display()))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(raw.trim())
        .context("private key is not valid base64")?;
    let arr: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("private key must decode to exactly 32 bytes"))?;
    Ok(SigningKey::from_bytes(&arr))
}

/// 公钥的 base64 文本形式（填入 host `TRUSTED_KEYS` 或传给 verify --pubkey）。
pub fn pubkey_base64(sk: &SigningKey) -> String {
    base64::engine::general_purpose::STANDARD.encode(sk.verifying_key().to_bytes())
}

/// 生成新密钥对（用操作系统 CSPRNG）。
pub fn generate_keypair() -> SigningKey {
    let mut seed = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut seed);
    SigningKey::from_bytes(&seed)
}

/// `sign` 的结果：用于 dry-run 报告与 inspect 回显。
#[derive(Debug)]
pub struct SignOutput {
    pub plugin_id: String,
    pub version: String,
    pub key_id: String,
    pub files: Vec<FileEntry>,
}

/// 对插件目录签名：读 `plugin.json` → 收集文件哈希 → 拼 canonical bytes →
/// Ed25519 签名 → 写 `signature.json`。`dry_run=true` 只报告文件清单不写盘。
pub fn sign_dir(dir: &Path, key_path: &Path, key_id: &str, dry_run: bool) -> Result<SignOutput> {
    let manifest = PluginManifest::load(&dir.join("plugin.json"))
        .with_context(|| format!("load {}", dir.join("plugin.json").display()))?;
    let sk = read_private_key(key_path)?;
    let files = collect_file_entries(dir)?;

    if dry_run {
        return Ok(SignOutput {
            plugin_id: manifest.id,
            version: manifest.version,
            key_id: key_id.to_string(),
            files,
        });
    }

    let canon = canonical_bytes(&manifest.id, &manifest.version, "ed25519", key_id, &files);
    let signature = sk.sign(&canon);
    let sig_b64 = base64::engine::general_purpose::STANDARD.encode(signature.to_bytes());

    let sig_file = SignatureFile {
        schema: 1,
        plugin_id: &manifest.id,
        version: &manifest.version,
        algorithm: "ed25519",
        key_id,
        signed_at: now_iso8601_utc(),
        files: &files,
        signature: sig_b64.clone(),
    };
    let json = serde_json::to_string_pretty(&sig_file)?;
    fs::write(dir.join("signature.json"), json)?;

    // 本地签名审计日志（规范 §9.2）：时间,key_id,plugin_id,version,文件数,签名前缀。
    // 落到私钥旁 `<key>.audit.log`（被 *.log 忽略，不入仓库），供私钥泄露后事后审计。
    append_audit_log(
        &key_path.with_extension("audit.log"),
        key_id,
        &manifest.id,
        &manifest.version,
        files.len(),
        &sig_b64,
    )?;

    Ok(SignOutput {
        plugin_id: manifest.id,
        version: manifest.version,
        key_id: key_id.to_string(),
        files,
    })
}

/// 追加一行签名审计日志（规范 §9.2）。写入失败会让 `sign_dir` 整体失败
/// （signature.json 此时可能已落盘——属部分完成态，重试会追加重复日志行，
/// 可接受；审计完整性优先于容错）。
fn append_audit_log(
    path: &Path,
    key_id: &str,
    plugin_id: &str,
    version: &str,
    file_count: usize,
    sig_b64: &str,
) -> Result<()> {
    let line = format!(
        "{ts},{key_id},{plugin_id},{version},{file_count},{sig_prefix}\n",
        ts = now_iso8601_utc(),
        sig_prefix = &sig_b64[..sig_b64.len().min(16)],
    );
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)?;
    f.write_all(line.as_bytes())?;
    Ok(())
}

/// 验签：`pubkey_b64=None` 用 host 内置 `TRUSTED_KEYS`；`Some(b64)` 用该公钥
/// （key_id 取自 `signature.json`，便于对任意开发密钥自检，规范 §9.1）。
///
/// 无 `signature.json` 时两种路径都返回 `Ok(Unsigned)`，CLI 语义一致。
pub fn verify_plugin(dir: &Path, pubkey_b64: Option<&str>) -> Result<SignState> {
    let manifest = PluginManifest::load(&dir.join("plugin.json"))?;
    // 无 signature.json：与内置路径一致，统一返回 Unsigned（不报错）。
    if !dir.join("signature.json").is_file() {
        return Ok(SignState::Unsigned);
    }
    match pubkey_b64 {
        None => Ok(verify_dir(dir, &manifest.id, &manifest.version)?),
        Some(b64) => {
            let key_id = read_signature_key_id(dir)?;
            // CLI 短生命周期进程，owned String 直接进 Cow::Owned 即可。
            let key = TrustedKey {
                key_id: std::borrow::Cow::Owned(key_id),
                algorithm: std::borrow::Cow::Borrowed("ed25519"),
                public_key: std::borrow::Cow::Owned(b64.to_string()),
                kind: KeyKind::Official,
                note: std::borrow::Cow::Borrowed("cli --pubkey"),
            };
            Ok(verify_with_keys(
                dir,
                &manifest.id,
                &manifest.version,
                std::slice::from_ref(&key),
                &[],
            )?)
        }
    }
}

/// 读取 `signature.json` 的 `key_id`（仅 verify --pubkey / inspect 用）。
pub fn read_signature_key_id(dir: &Path) -> Result<String> {
    let raw = fs::read_to_string(dir.join("signature.json")).context("signature.json not found")?;
    let v: serde_json::Value = serde_json::from_str(&raw)?;
    v.get("key_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .context("signature.json missing key_id")
}

/// 读取 `signature.json` 原文（inspect 打印用）。
pub fn read_signature_raw(dir: &Path) -> Result<String> {
    Ok(fs::read_to_string(dir.join("signature.json"))?)
}

// ─── check-registry：官方仓库准入校验（规范 §12.2 3.1 + §4.6）─────────────

/// `check-registry` 的校验结果。`errors` 非空即准入失败（CLI 退出码 1）。
#[derive(Debug, Default)]
pub struct RegistryCheck {
    /// 已检查的版本条目总数。
    pub versions: usize,
    /// 准入失败项（必须修掉才可发布/合并）。
    pub errors: Vec<String>,
    /// 非阻断提示（如 registry signature 与包内不一致，以包内为准）。
    pub warnings: Vec<String>,
}

/// 校验官方仓库 `registry.json` 的 3.1 准入规则：
///
/// 1. **新版本必须带有效签名**：每个 `versions[]` 条目必须有 `signature` 对象，
///    且 schema==1 / algorithm=="ed25519" / key_id 命中可信官方密钥 /
///    signature 能解码为 64 字节 Ed25519 签名。
/// 2. **包内验签（可选，`--dir` 指定仓库根）**：对每个 `path` 指向的本地目录做
///    全量 `verify_dir`，包内 `signature.json` 必须验为 `Official`（不能是
///    Unsigned/Invalid——包内是权威，registry 字段仅作展示与预检，§4.6）。
/// 3. registry `signature` 与包内不一致 → 仅警告（以包内为准，但也应同步修正）。
///
/// 校验用 `keys`（默认内置 `TRUSTED_KEYS`）判定"官方"：key_id 白名单与验签同表。
pub fn check_registry(registry_path: &Path, repo_root: Option<&Path>) -> Result<RegistryCheck> {
    check_registry_with_keys(registry_path, repo_root, TRUSTED_KEYS, TRUSTED_KEYS)
}

/// 校验注册表，用显式传入的可信表（测试/自定义仓库用）。`allow_ids` 是允许出现在
/// `versions[].signature.key_id` 的官方 key_id 集合（验签与白名单默认同表）。
pub fn check_registry_with_keys(
    registry_path: &Path,
    repo_root: Option<&Path>,
    keys: &[TrustedKey],
    allow_ids: &[TrustedKey],
) -> Result<RegistryCheck> {
    let raw = fs::read_to_string(registry_path)
        .with_context(|| format!("read {}", registry_path.display()))?;
    let reg: Registry =
        serde_json::from_str(&raw).context("registry.json 格式不符《插件市场与仓库.md》§3")?;
    if reg.schema != 1 {
        bail!(
            "registry.schema 必须为 1，实际 {}（不支持前向格式）",
            reg.schema
        );
    }

    let mut check = RegistryCheck::default();
    let official_ids: Vec<&str> = allow_ids
        .iter()
        .filter(|k| k.kind == KeyKind::Official)
        .map(|k| k.key_id.as_ref())
        .collect();

    for plugin in &reg.plugins {
        if plugin.versions.is_empty() {
            check
                .warnings
                .push(format!("{}: 无任何 versions（空条目）", plugin.id));
        }
        for v in &plugin.versions {
            check.versions += 1;
            let label = format!("{}@{}", plugin.id, v.version);
            // 1) 签名结构（3.1 强制）
            let Some(sig) = &v.signature else {
                check.errors.push(format!(
                    "{label}: 未签名——3.1 准入规则要求官方仓库每个版本必须带 signature"
                ));
                continue;
            };
            if sig.schema != 1 {
                check.errors.push(format!(
                    "{label}: signature.schema 必须为 1，实际 {}",
                    sig.schema
                ));
                continue;
            }
            if sig.algorithm != "ed25519" {
                check.errors.push(format!(
                    "{label}: signature.algorithm 必须为 ed25519，实际 {}",
                    sig.algorithm
                ));
                continue;
            }
            if !official_ids.iter().any(|id| *id == sig.key_id) {
                check.errors.push(format!(
                    "{label}: signature.key_id={} 不是官方密钥（仅接受 {}）",
                    sig.key_id,
                    official_ids.join(" / ")
                ));
                continue;
            }
            match base64::engine::general_purpose::STANDARD.decode(sig.signature.trim()) {
                Ok(bytes) if bytes.len() == 64 => {}
                Ok(bytes) => check.errors.push(format!(
                    "{label}: signature 必须解码为 64 字节，实际 {}",
                    bytes.len()
                )),
                Err(_) => check
                    .errors
                    .push(format!("{label}: signature 不是合法 base64")),
            }

            // 2) 包内验签（--dir 给仓库根时）
            if let (Some(root), Some(path)) = (repo_root, v.path.as_deref()) {
                let pkg_dir = root.join(path);
                if pkg_dir.is_dir() {
                    match verify_with_keys(&pkg_dir, &plugin.id, &v.version, keys, &[]) {
                        Ok(SignState::Official) => {
                            // 3) registry 字段与包内一致性（包内为权威，§4.6）
                            if let Some((pkg_key_id, pkg_sig)) = read_package_signature(&pkg_dir) {
                                if pkg_key_id != sig.key_id {
                                    check.warnings.push(format!(
                                        "{label}: registry signature.key_id={} 与包内 {} 不一致（以包内为准）",
                                        sig.key_id, pkg_key_id
                                    ));
                                }
                                if pkg_sig != sig.signature.trim() {
                                    check.warnings.push(format!(
                                        "{label}: registry signature 与包内 signature.json 不一致（以包内为准，建议同步修正 registry）"
                                    ));
                                }
                            }
                        }
                        Ok(SignState::Unsigned) => check.errors.push(format!(
                            "{label}: 包内缺 signature.json——包内必须带签名（registry 字段不替代包内权威）"
                        )),
                        Ok(SignState::Invalid) => check.errors.push(format!(
                            "{label}: 包内验签失败（文件被改/签名不可信/与可信表不符），准入拒绝"
                        )),
                        Ok(state) => check.errors.push(format!(
                            "{label}: 包内验签结果为 {state:?}，官方仓库只接受 Official"
                        )),
                        Err(e) => check.errors.push(format!("{label}: 包内验签 io 错误：{e}")),
                    }
                } else if v.url.as_deref().is_none() {
                    check.errors.push(format!(
                        "{label}: path={path} 在本地仓库根下不存在（且 url 为空，条目悬空）"
                    ));
                } else {
                    check.warnings.push(format!(
                        "{label}: 本地无 {path}（url 指向外部包），跳过包内验签"
                    ));
                }
            }
        }
    }
    Ok(check)
}

/// 读包内 `signature.json` 的 (key_id, signature)，供与 registry 字段交叉比对。
fn read_package_signature(dir: &Path) -> Option<(String, String)> {
    let raw = fs::read_to_string(dir.join("signature.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    Some((
        v.get("key_id")?.as_str()?.to_string(),
        v.get("signature")?.as_str()?.to_string(),
    ))
}

/// registry.json 的按需子集（schema / plugins[].versions[]，规范 §3）。
#[derive(Deserialize)]
struct Registry {
    schema: u32,
    plugins: Vec<RegistryPlugin>,
}

#[derive(Deserialize)]
struct RegistryPlugin {
    id: String,
    #[serde(default)]
    versions: Vec<RegistryVersion>,
}

#[derive(Deserialize)]
struct RegistryVersion {
    version: String,
    path: Option<String>,
    url: Option<String>,
    /// 缺省（未签名）与 null 同义：3.1 起官方仓库每个版本必须带。
    #[serde(default)]
    signature: Option<RegistrySignature>,
}

#[derive(Deserialize)]
struct RegistrySignature {
    schema: u32,
    key_id: String,
    algorithm: String,
    signature: String,
}

/// 当前 UTC 时间的 ISO 8601 字符串（`signed_at` 记录用，不参与验签）。
fn now_iso8601_utc() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (y, m, d, hh, mm, ss) = civil_from_unix(secs);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Unix 秒 → UTC 年月日时分秒（Howard Hinnant 算法）。
fn civil_from_unix(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let hh = (rem / 3600) as u32;
    let mm = ((rem % 3600) / 60) as u32;
    let ss = (rem % 60) as u32;

    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = (z - era * 146097) as i64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let mut y = era * 400 + yoe;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    if m <= 2 {
        y += 1;
    }
    (y, m, d, hh, mm, ss)
}

/// `signature.json` 序列化结构（字段顺序即输出顺序）。
#[derive(Serialize)]
struct SignatureFile<'a> {
    schema: u32,
    plugin_id: &'a str,
    version: &'a str,
    algorithm: &'a str,
    key_id: &'a str,
    signed_at: String,
    files: &'a [FileEntry],
    signature: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_key_roundtrip() {
        let sk = generate_keypair();
        let tmp = std::env::temp_dir().join("spark_sign_key_io");
        let _ = fs::remove_dir_all(&tmp);
        let path = tmp.join("k.key");
        write_private_key(&path, &sk).unwrap();
        let sk2 = read_private_key(&path).unwrap();
        assert_eq!(sk.to_bytes(), sk2.to_bytes());
    }

    #[test]
    fn pubkey_base64_is_32_bytes() {
        let sk = generate_keypair();
        let b64 = pubkey_base64(&sk);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .unwrap();
        assert_eq!(bytes.len(), 32);
    }

    #[test]
    fn civil_from_unix_epoch_is_1970() {
        let (y, m, d, hh, mm, ss) = civil_from_unix(0);
        assert_eq!((y, m, d, hh, mm, ss), (1970, 1, 1, 0, 0, 0));
    }

    #[test]
    fn civil_from_known_timestamp() {
        // 2026-08-25T10:00:00Z = 1787652000
        let (y, m, d, hh, mm, ss) = civil_from_unix(1787652000);
        assert_eq!((y, m, d, hh, mm, ss), (2026, 8, 25, 10, 0, 0));
    }
}
