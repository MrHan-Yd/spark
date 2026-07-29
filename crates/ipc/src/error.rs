use thiserror::Error;

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid message: {0}")]
    Invalid(String),
}
