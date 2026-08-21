use thiserror::Error;

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid message: {0}")]
    Invalid(String),
}
