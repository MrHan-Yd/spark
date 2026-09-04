//! JSON-RPC style messages over Named Pipe (see docs/DESIGN.md).
//!
//! Host ↔ UI 走 NDJSON（`encode_line`/`decode_line`）；Host ↔ Native 插件走
//! length-prefixed 帧（`frame` 模块，4 字节小端长度 + UTF-8 JSON）。

mod error;
mod frame;
mod message;
mod protocol;

pub use error::IpcError;
pub use frame::{read_frame, write_frame};
pub use message::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};
pub use protocol::{
    HostMethod, InvokeParams, InvokeResult, JsonRpcNotification, PluginApiParams,
    PluginDevLoadParams, PluginGrantParams, PluginIdParams, PluginInitializeParams,
    PluginInitializeResult, PluginInstallParams, PluginMethod, PluginOpenParams, PluginPageParams,
    PluginSetDirParams, PluginToggleParams, QueryParams, QueryResult, SetConfigParams,
    TrustedPubkeyEntry, UiMethod, API_VERSION, PIPE_NAME, PIPE_PATH,
};

/// Encode one NDJSON line (without trailing newline applied by caller if needed).
pub fn encode_line<T: serde::Serialize>(value: &T) -> Result<String, IpcError> {
    Ok(serde_json::to_string(value)?)
}

pub fn decode_line<T: serde::de::DeserializeOwned>(line: &str) -> Result<T, IpcError> {
    Ok(serde_json::from_str(line.trim())?)
}
