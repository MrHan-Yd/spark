//! 内置可信公钥表与吊销列表（规范 §5、§8）。
//!
//! 编译期硬编码进 host 二进制的**官方**公钥表与吊销列表（不读外部文件、不可运行时改）。
//! 验签公钥**恒来自可信表**：`signature.json` 里的 `key_id` 仅作索引，不被信任为公钥来源。
//! 三方（3.3）用户导入的公钥走 `HostConfig.trusted_pubkeys` → `PluginManager` 的运行时
//! 合并表（`set_trusted_user_keys`），`KeyKind::ThirdParty`，并入后与内置表一同参与验签。
//!
//! **官方密钥生成**：在离线机上跑 `spark-sign keygen --out <keyfile> --key-id spark-official-v1`，
//! 把打印的 base64 公钥填到下面对应条目的 `public_key`，私钥按规范 §5.2 保管（不入仓库、
//! 不进产物，放 CI secret）。轮换时追加 `v2`，overlap 期同表共存（§8.1）。

use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// 可信密钥种类：决定验过后 UI 角标是"官方"还是"已签名"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyKind {
    /// 官方签名密钥（内置表，验过即标"官方插件"）。
    Official,
    /// 第三方签名密钥（用户从设置页导入的开发者公钥，验过即标"已签名"）。
    ThirdParty,
}

/// 一条可信公钥。官方条目编译进 host 二进制（`Cow::Borrowed`）；
/// 三方条目由设置页导入（`Cow::Owned`）挂在运行时合并表里。
#[derive(Debug, Clone)]
pub struct TrustedKey {
    pub key_id: Cow<'static, str>,
    pub algorithm: Cow<'static, str>,
    /// base64 编码的 32 字节 Ed25519 公钥。
    pub public_key: Cow<'static, str>,
    pub kind: KeyKind,
    pub note: Cow<'static, str>,
}

/// host 内置官方公钥表。
///
/// 轮换时在此追加 `v2`，overlap 期同表共存（规范 §8.1）。
/// 用户导入的三方密钥**不在此表**，由 `PluginManager::set_trusted_user_keys` 合并
/// （`KeyKind::ThirdParty`）；此表恒为官方密钥来源，"官方"判定不受用户导入影响。
pub const TRUSTED_KEYS: &[TrustedKey] = &[TrustedKey {
    key_id: Cow::Borrowed("spark-official-v1"),
    algorithm: Cow::Borrowed("ed25519"),
    // ⚠️ 开发密钥：用本机 `spark-sign keygen` 生成，私钥在 keys/spark-official-v1.key
    // （gitignored）。**正式发布前必须在离线机重新生成**，把新公钥填回此处，
    // 新私钥放 CI secret（不入仓库、不进产物）。见规范 §5.2。
    public_key: Cow::Borrowed("3YCEUQRYeqCia6kdGqlUU8bCPOboppg/Z8z6OQLwTdo="),
    kind: KeyKind::Official,
    note: Cow::Borrowed("Spark 官方签名密钥 v1（开发密钥，发布前需离线机重生成）"),
}];

/// 本地吊销列表（编译期，随 host 版本更新）。
///
/// 粒度可吊销整把 `key_id`（私钥泄露）或单 `(plugin_id, version)`（单版本恶意）。
/// 验签时命中即视为 `SignState::Invalid`（规范 §8.2）。表保持为空：
/// **吊销是应急动作**——发现恶意/泄露后随 host 紧急版本把条目加进此表发布。
/// 示例（私钥泄露）：
/// ```text
/// Revocation { key_id: "spark-official-v1", plugin_id: None, version: None, reason: "key leak" }
/// ```
pub const REVOKED: &[Revocation] = &[];

#[derive(Debug, Clone)]
pub struct Revocation {
    pub key_id: &'static str,
    /// `None` 表示吊销该 key_id 签的所有插件。
    pub plugin_id: Option<&'static str>,
    /// `None` 表示吊销该插件所有版本。
    pub version: Option<&'static str>,
    pub reason: &'static str,
}

/// 判断 `key_id` 是否命中吊销列表。
///
/// 命中规则：`key_id` 相同，且 `plugin_id`/`version` 为 `None` 或精确匹配。
pub fn is_revoked(revoked: &[Revocation], key_id: &str, plugin_id: &str, version: &str) -> bool {
    revoked.iter().any(|r| {
        r.key_id == key_id
            && r.plugin_id.map_or(true, |p| p == plugin_id)
            && r.version.map_or(true, |v| v == version)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revoked_key_scope_matches_all_plugins() {
        let table = &[Revocation {
            key_id: "k1",
            plugin_id: None,
            version: None,
            reason: "leak",
        }];
        assert!(is_revoked(table, "k1", "com.spark.a", "0.1.0"));
        assert!(is_revoked(table, "k1", "com.spark.b", "9.9.9"));
        assert!(!is_revoked(table, "k2", "com.spark.a", "0.1.0"));
    }

    #[test]
    fn revoked_single_version_scoped() {
        let table = &[Revocation {
            key_id: "k1",
            plugin_id: Some("com.spark.a"),
            version: Some("0.2.0"),
            reason: "malware",
        }];
        assert!(is_revoked(table, "k1", "com.spark.a", "0.2.0"));
        // 同插件其他版本不受影响。
        assert!(!is_revoked(table, "k1", "com.spark.a", "0.1.0"));
        // 不同插件不受影响。
        assert!(!is_revoked(table, "k1", "com.spark.b", "0.2.0"));
    }

    #[test]
    fn empty_revoked_table_matches_nothing() {
        assert!(!is_revoked(
            REVOKED,
            "spark-official-v1",
            "com.spark.x",
            "0.1.0"
        ));
    }
}
