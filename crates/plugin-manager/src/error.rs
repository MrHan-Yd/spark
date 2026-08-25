use thiserror::Error;

use crate::signing::VerifyError;

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid manifest: {0}")]
    Manifest(String),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    /// 验签过程的文件系统层错误（目录不可读等）。签名内容问题不在此列
    /// （映射为 `SignatureInvalid`）。
    #[error("verify io error: {0}")]
    Verify(#[from] VerifyError),
    /// 插件有 `signature.json` 但验签失败（哈希不匹配/签名不过/格式错/key_id 不可信）。
    /// install 侧策略：破损签名一律拒装（规范 §6.2）。
    #[error("plugin signature invalid: {0}")]
    SignatureInvalid(String),
    /// 插件无 `signature.json` 且调用方要求强制签名（`require_signature=true`）。
    #[error("plugin signature missing but required: {0}")]
    SignatureMissing(String),
}
