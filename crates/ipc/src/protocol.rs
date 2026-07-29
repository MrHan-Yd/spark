use serde::{Deserialize, Serialize};
use spark_core::Candidate;

/// Wire protocol major version (bump on breaking changes).
pub const API_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostMethod {
    Query,
    Invoke,
    GetConfig,
    SetConfig,
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
    Close { message: Option<String> },
    Keep { message: Option<String> },
    CopyText { text: String },
    OpenUrl { url: String },
    ShowError { message: String },
}
