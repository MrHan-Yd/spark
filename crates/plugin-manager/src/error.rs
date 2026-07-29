use thiserror::Error;

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid manifest: {0}")]
    Manifest(String),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}
