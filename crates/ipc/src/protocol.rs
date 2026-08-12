use serde::{Deserialize, Serialize};
use spark_core::Candidate;

/// Wire protocol major version (bump on breaking changes).
pub const API_VERSION: u32 = 1;

/// Named pipe bare name (C# `NamedPipeClientStream(".", name, …)`).
pub const PIPE_NAME: &str = "spark.host.ipc";

/// Full Win32 path for CreateNamedPipe / CreateFile.
pub const PIPE_PATH: &str = r"\\.\pipe\spark.host.ipc";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostMethod {
    Query,
    Invoke,
    GetConfig,
    SetConfig,
    GetBuiltins,
    Toggle,
    Show,
    Hide,
}

impl HostMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Query => "host.query",
            Self::Invoke => "host.invoke",
            Self::GetConfig => "host.get_config",
            Self::SetConfig => "host.set_config",
            Self::GetBuiltins => "host.get_builtins",
            Self::Toggle => "host.toggle",
            Self::Show => "ui.show",
            Self::Hide => "ui.hide",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginMethod {
    Initialize,
    Shutdown,
    Query,
    Invoke,
    Cancel,
}

impl PluginMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Initialize => "plugin.initialize",
            Self::Shutdown => "plugin.shutdown",
            Self::Query => "plugin.query",
            Self::Invoke => "plugin.invoke",
            Self::Cancel => "plugin.cancel",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiMethod {
    Show,
    Hide,
    SetQuery,
    Results,
    Toggle,
}

impl UiMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Show => "ui.show",
            Self::Hide => "ui.hide",
            Self::SetQuery => "ui.set_query",
            Self::Results => "ui.results",
            Self::Toggle => "ui.toggle",
        }
    }
}

/// JSON-RPC notification (no `id`) for Host → UI push.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

impl JsonRpcNotification {
    pub fn new(method: impl Into<String>, params: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            method: method.into(),
            params,
        }
    }

    pub fn ui_show() -> Self {
        Self::new(UiMethod::Show.as_str(), serde_json::json!({}))
    }

    pub fn ui_hide() -> Self {
        Self::new(UiMethod::Hide.as_str(), serde_json::json!({}))
    }

    pub fn ui_toggle() -> Self {
        Self::new(UiMethod::Toggle.as_str(), serde_json::json!({}))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryParams {
    pub text: String,
    #[serde(default = "default_limit")]
    pub limit: u32,
}

fn default_limit() -> u32 {
    50
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    pub items: Vec<Candidate>,
    #[serde(default)]
    pub partial: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvokeParams {
    pub item_id: String,
    pub action_id: String,
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InvokeResult {
    Close {
        message: Option<String>,
    },
    Keep {
        message: Option<String>,
    },
    CopyText {
        text: String,
    },
    OpenUrl {
        url: String,
    },
    ShowError {
        message: String,
    },
    /// 不可逆操作（关机/重启等）的确认请求：UI 弹窗，确认后以
    /// `action_id = "confirm"` 重新 invoke 才真正执行。
    Confirm {
        message: String,
    },
}
