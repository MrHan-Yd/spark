use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
}
