//! JSON-RPC style messages over Named Pipe (see docs/DESIGN.md).

mod error;
mod message;
mod protocol;

pub use error::IpcError;
pub use message::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};
pub use protocol::{
    HostMethod, InvokeParams, InvokeResult, PluginMethod, QueryParams, QueryResult, UiMethod,
    API_VERSION,
};

/// Encode one NDJSON line (without trailing newline applied by caller if needed).
pub fn encode_line<T: serde::Serialize>(value: &T) -> Result<String, IpcError> {
    Ok(serde_json::to_string(value)?)
}

pub fn decode_line<T: serde::de::DeserializeOwned>(line: &str) -> Result<T, IpcError> {
    Ok(serde_json::from_str(line.trim())?)
}
