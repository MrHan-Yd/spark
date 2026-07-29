//! Minimal plugin SDK surface for authors (stdio / pipe loop filled in later).

use serde_json::Value;
use spark_core::Candidate;
use spark_ipc::{InvokeParams, InvokeResult, QueryParams, QueryResult};

pub trait Plugin {
    fn id(&self) -> &str;
    fn query(&mut self, params: QueryParams) -> QueryResult;
    fn invoke(&mut self, params: InvokeParams) -> InvokeResult;
}

/// Helper: single-item list result.
pub fn single_item(item: Candidate) -> QueryResult {
    QueryResult {
        items: vec![item],
        partial: false,
    }
}

pub fn empty_result() -> QueryResult {
    QueryResult {
        items: vec![],
        partial: false,
    }
}

/// Placeholder entry for plugins that will speak JSON-RPC on a pipe.
pub fn sdk_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn parse_params<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, serde_json::Error> {
    serde_json::from_value(value)
}
