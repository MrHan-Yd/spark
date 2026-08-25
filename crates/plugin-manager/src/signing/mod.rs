//! 插件代码签名（三期）：内容清单签名 + Ed25519 验签。
//!
//! 规范见 [`插件开发/插件签名规范.md`]。本模块只做**纯逻辑**：
//! - `canon`：canonical bytes 拼装 + 文件清单收集（sign-tool 与 host 共用，保证两端字节一致）
//! - `verify`：读 `signature.json`、重算**全部**文件哈希（3.2 起全量，install 与 scan 同路）、Ed25519 验签
//! - `keys`：内置官方公钥表与吊销列表（编译期硬编码，不可运行时改）
//!
//! 验签公钥来源：内置 `TRUSTED_KEYS`（官方）+ `PluginManager` 运行时合并的用户导入
//! 三方密钥（`KeyKind::ThirdParty`，来自 `HostConfig.trusted_pubkeys`）。
//! "官方插件"判定 = 验签通过 且 key_id 命中 `TRUSTED_KEYS` 中 `KeyKind::Official` 条目，
//! **不是**清单里的自报字段。不新增 IPC 方法；验签是 install/scan 的内部步骤。

mod canon;
mod keys;
mod verify;

pub use canon::{canonical_bytes, collect_file_entries, FileEntry};
pub use keys::{is_revoked, KeyKind, Revocation, TrustedKey, REVOKED, TRUSTED_KEYS};
pub use verify::{verify_dir, verify_with_keys, SignState, VerifyError};
