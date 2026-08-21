//! Spark 官方 Rust SDK：native 插件入口。
//!
//! 插件作者实现 [`Plugin`] trait，main 里调 [`run_loop`] 即可。
//! `run_loop` 负责：帧编解码、`plugin.initialize` 握手、`query`/`invoke`/`cancel`
//! 分发、`plugin.shutdown` 退出。stdout 必须纯净协议帧，日志走 stderr。

use serde_json::Value;
use spark_core::Candidate;
use spark_ipc::{
    decode_line, encode_line, read_frame, write_frame, InvokeParams, InvokeResult, JsonRpcRequest,
    JsonRpcResponse, PluginInitializeParams, PluginInitializeResult, PluginMethod, QueryParams,
    QueryResult,
};
use std::io::{self, BufReader};
use thiserror::Error;

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

pub fn sdk_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn parse_params<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, serde_json::Error> {
    serde_json::from_value(value)
}

/// `run_loop` / `dispatch_request` 的失败原因。
#[derive(Debug, Error)]
pub enum RunLoopError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("frame codec error: {0}")]
    Frame(#[from] spark_ipc::IpcError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    /// stdin 干净关闭（host 退出）：正常结束，非错误路径但用 Result 表达。
    #[error("stdin closed")]
    Closed,
}

/// 阻塞运行插件主循环：从 stdin 读帧 → 分发 → 向 stdout 写响应。
///
/// 返回 `Ok(())` 仅在收到 `plugin.shutdown` 或 stdin 干净关闭时；
/// 其他 IO/协议错误以 `Err` 上抛，由 main 决定是否退出进程。
pub fn run_loop(plugin: &mut dyn Plugin) -> Result<(), RunLoopError> {
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = io::stdout();
    let mut writer = stdout.lock();

    loop {
        let body = match read_frame(&mut reader)? {
            Some(b) => b,
            None => return Ok(()),
        };
        let req: JsonRpcRequest = decode_line(std::str::from_utf8(&body).unwrap_or(""))?;
        match dispatch_request(plugin, req)? {
            DispatchOutcome::Reply(resp) => {
                let line = encode_line(&resp)?;
                write_frame(&mut writer, line.as_bytes())?;
            }
            DispatchOutcome::NoReply => {}
            DispatchOutcome::Shutdown => return Ok(()),
        }
    }
}

/// 单次请求的处理结果：多数方法回一条响应，`shutdown`(notification) 触发循环退出，
/// `cancel` 是 notification 不回帧。
enum DispatchOutcome {
    Reply(JsonRpcResponse),
    NoReply,
    Shutdown,
}

/// 处理一条 JSON-RPC 请求，返回应答（或 Shutdown 信号）。
///
/// 抽出来独立于 stdin/stdout，便于单测：`run_loop` 仅负责 IO，本函数负责协议语义。
fn dispatch_request(
    plugin: &mut dyn Plugin,
    req: JsonRpcRequest,
) -> Result<DispatchOutcome, RunLoopError> {
    // notification（无 id）：仅 cancel 有意义，忽略其余。
    let id = req.id.clone();
    let is_notification = id.is_none();

    let resp = match req.method.as_str() {
        m if m == PluginMethod::Initialize.as_str() => {
            let params: PluginInitializeParams = serde_json::from_value(req.params)?;
            // 插件应确认自己被声明的 id；不一致仍继续，避免一上线就硬崩。
            if params.id != plugin.id() {
                eprintln!(
                    "spark-sdk: initialize id mismatch (host={:?} plugin={:?})",
                    params.id,
                    plugin.id()
                );
            }
            let result = PluginInitializeResult {
                plugin_id: plugin.id().to_string(),
                sdk_version: sdk_version().to_string(),
            };
            JsonRpcResponse::result(id, serde_json::to_value(result)?)
        }
        m if m == PluginMethod::Query.as_str() => {
            let params: QueryParams = serde_json::from_value(req.params)?;
            let result = plugin.query(params);
            JsonRpcResponse::result(id, serde_json::to_value(result)?)
        }
        m if m == PluginMethod::Invoke.as_str() => {
            let params: InvokeParams = serde_json::from_value(req.params)?;
            let result = plugin.invoke(params);
            JsonRpcResponse::result(id, serde_json::to_value(result)?)
        }
        m if m == PluginMethod::Cancel.as_str() => {
            // cancel 是 notification（无 id）：不回帧，保持 stdout 纯净。
            // 一期插件不实现真正的中断；host 也不会等待 cancel 响应。
            if !is_notification {
                eprintln!("spark-sdk: cancel should be a notification (no id)");
            }
            return Ok(DispatchOutcome::NoReply);
        }
        m if m == PluginMethod::Shutdown.as_str() => {
            // shutdown 可以带 id（host 等待确认）或作 notification。
            if let Some(rid) = id.clone() {
                let ack = JsonRpcResponse::result(Some(rid), Value::Null);
                return Ok(DispatchOutcome::Reply(ack));
            }
            return Ok(DispatchOutcome::Shutdown);
        }
        other => JsonRpcResponse::error(id, -32601, format!("method not found: {other}")),
    };
    Ok(DispatchOutcome::Reply(resp))
}

#[cfg(test)]
mod tests {
    use super::*;
    use spark_core::{Action, Source};

    struct Echo;

    impl Plugin for Echo {
        fn id(&self) -> &str {
            "com.spark.echo"
        }
        fn query(&mut self, params: QueryParams) -> QueryResult {
            single_item(Candidate {
                id: format!("echo:{}", params.text),
                title: params.text,
                subtitle: Some("Echo".into()),
                target: None,
                icon: None,
                score: 1.0,
                source: Source::Plugin,
                actions: vec![Action {
                    id: "copy".into(),
                    title: "复制".into(),
                    is_default: true,
                    target: None,
                }],
                plugin_id: Some(self.id().into()),
            })
        }
        fn invoke(&mut self, params: InvokeParams) -> InvokeResult {
            InvokeResult::CopyText { text: params.text }
        }
    }

    fn req(method: &str, id: Option<i64>, params: Value) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: id.map(Value::from),
            method: method.into(),
            params,
        }
    }

    #[test]
    fn dispatch_initialize_replies_with_sdk_version() {
        let mut p = Echo;
        let r = dispatch_request(
            &mut p,
            req(
                PluginMethod::Initialize.as_str(),
                Some(1),
                serde_json::json!({ "id": "com.spark.echo", "permissions": [], "api_version": 1 }),
            ),
        )
        .unwrap();
        match r {
            DispatchOutcome::Reply(resp) => {
                let result = resp.result.unwrap();
                assert_eq!(result["plugin_id"], "com.spark.echo");
                assert!(result["sdk_version"].is_string());
            }
            DispatchOutcome::Shutdown | DispatchOutcome::NoReply => panic!("expected reply"),
        }
    }

    #[test]
    fn dispatch_query_returns_items() {
        let mut p = Echo;
        let r = dispatch_request(
            &mut p,
            req(
                PluginMethod::Query.as_str(),
                Some(2),
                serde_json::json!({ "text": "hi", "limit": 10 }),
            ),
        )
        .unwrap();
        match r {
            DispatchOutcome::Reply(resp) => {
                let result = resp.result.unwrap();
                assert_eq!(result["items"][0]["id"], "echo:hi");
            }
            DispatchOutcome::Shutdown | DispatchOutcome::NoReply => panic!("expected reply"),
        }
    }

    #[test]
    fn dispatch_invoke_returns_copy_text() {
        let mut p = Echo;
        let r = dispatch_request(
            &mut p,
            req(
                PluginMethod::Invoke.as_str(),
                Some(3),
                serde_json::json!({ "item_id": "echo:hi", "action_id": "copy", "text": "hi" }),
            ),
        )
        .unwrap();
        match r {
            DispatchOutcome::Reply(resp) => {
                let result = resp.result.unwrap();
                assert_eq!(result["type"], "copy_text");
                assert_eq!(result["text"], "hi");
            }
            DispatchOutcome::Shutdown | DispatchOutcome::NoReply => panic!("expected reply"),
        }
    }

    #[test]
    fn dispatch_shutdown_notification_exits_loop() {
        let mut p = Echo;
        let r = dispatch_request(
            &mut p,
            req(PluginMethod::Shutdown.as_str(), None, Value::Null),
        )
        .unwrap();
        assert!(matches!(r, DispatchOutcome::Shutdown));
    }

    #[test]
    fn dispatch_unknown_method_returns_error() {
        let mut p = Echo;
        let r = dispatch_request(&mut p, req("plugin.bogus", Some(9), Value::Null)).unwrap();
        match r {
            DispatchOutcome::Reply(resp) => {
                assert!(resp.error.is_some());
            }
            DispatchOutcome::Shutdown | DispatchOutcome::NoReply => panic!("expected reply"),
        }
    }
}
