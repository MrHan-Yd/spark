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
    // 插件管理（设置页 + 主搜索开窗 + spark.* 桥）
    PluginList,
    PluginInstall,
    PluginUninstall,
    PluginToggle,
    PluginGrant,
    PluginDevLoad,
    PluginOpen,
    PluginApi,
    PluginSetDir,
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
            Self::PluginList => "host.plugin.list",
            Self::PluginInstall => "host.plugin.install",
            Self::PluginUninstall => "host.plugin.uninstall",
            Self::PluginToggle => "host.plugin.toggle",
            Self::PluginGrant => "host.plugin.grant",
            Self::PluginDevLoad => "host.plugin.devload",
            Self::PluginOpen => "host.plugin.open",
            Self::PluginApi => "host.plugin.api",
            Self::PluginSetDir => "host.plugin.set_dir",
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct InvokeParams {
    pub item_id: String,
    pub action_id: String,
    #[serde(default)]
    pub text: String,
}

/// `host.set_config` 参数：UI 设置页改动推送（缺省字段不动）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetConfigParams {
    /// 唤起热键（"Alt+Space"/"Ctrl+Space"…），变更后 host 重注册全局热键。
    #[serde(default)]
    pub hotkey_toggle: Option<String>,
    #[serde(default)]
    pub hide_on_focus_lost: Option<bool>,
    #[serde(default)]
    pub hide_on_execute: Option<bool>,
    #[serde(default)]
    pub launch_on_startup: Option<bool>,
    /// 严格模式（规范 §12.2 3.2）：开启后本地导入/市场安装均要求插件带有效签名，
    /// 无签名拒装（`SignatureMissing`）。默认关。
    #[serde(default)]
    pub strict_mode: Option<bool>,
    /// 全量替换"受信任开发者"三方公钥表（规范 §10 / §5.3）。缺省/null 不动。
    /// host 侧逐条校验（base64 32 字节公钥等），任一条非法即整体拒绝本次更新。
    #[serde(default)]
    pub trusted_pubkeys: Option<Vec<TrustedPubkeyEntry>>,
    /// 全量替换插件市场仓库 URL 列表（规范 §6 / §7）。缺省/null 不动。
    #[serde(default)]
    pub plugin_registry_urls: Option<Vec<String>>,
}

/// 一条用户导入的受信任三方密钥（`HostConfig.trusted_pubkeys` 的元素）。
/// 仅承载 key_id + 公钥 + 备注；验签时 `KeyKind` 恒为 `ThirdParty`（host 侧硬编码，
/// 不从此结构解析种类——防配置被篡改把三方密钥抬升为官方）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedPubkeyEntry {
    /// 开发者公钥标识（写入 signature.json 的 key_id；须全局唯一、非空）。
    pub key_id: String,
    /// base64 编码的 32 字节 Ed25519 公钥。
    pub public_key: String,
    /// 展示用备注（开发者名/主页等）。
    #[serde(default)]
    pub note: String,
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

// ─── 插件管理参数（host.plugin.*）─────────────────────────────────────────

/// `plugin.initialize` 请求参数（host → 插件握手）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginInitializeParams {
    /// 插件清单 id（host 侧已校验）。
    pub id: String,
    /// 用户已授予的权限列表（与清单声明取交集后）。
    #[serde(default)]
    pub permissions: Vec<String>,
    /// host 侧 wire 协议版本（`API_VERSION`）。
    pub api_version: u32,
}

/// `plugin.initialize` 响应：插件回报自身 SDK 版本与就绪状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInitializeResult {
    pub plugin_id: String,
    pub sdk_version: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginIdParams {
    pub id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginInstallParams {
    /// 待导入的源目录绝对路径（含 plugin.json）。
    pub path: String,
    /// true 时强制覆盖（用于降级确认后重试）；默认 false。
    /// false 且检测到旧版本时 host 返回 `confirm_downgrade` 而不写盘。
    #[serde(default)]
    pub force: bool,
    /// true 时要求源目录带有效 `signature.json`：无签名拒装（`SignatureMissing`），
    /// 签名破损一律拒装（`SignatureInvalid`）。默认 false，老 UI 不传仍兼容。
    #[serde(default)]
    pub require_signature: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginToggleParams {
    pub id: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginGrantParams {
    pub id: String,
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginDevLoadParams {
    /// 开发目录绝对路径（含 plugin.json；不拷贝）。
    pub dir: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginOpenParams {
    pub id: String,
    /// 去掉关键字前缀后的用户输入。
    #[serde(default)]
    pub input: String,
    /// 触发的关键字。
    #[serde(default)]
    pub command: String,
}

/// `host.plugin.api`：spark.* 特权能力桥（UI 的 WebView2 → host 执行）。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginApiParams {
    pub plugin_id: String,
    /// clipboard | notify | db | net | fs | shell
    pub capability: String,
    /// read_text / write_text / show / set / get ...
    pub method: String,
    #[serde(default)]
    pub args: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginSetDirParams {
    /// 新插件目录绝对路径。
    pub path: String,
    /// 是否迁移现有插件到新目录。
    #[serde(default)]
    pub migrate: bool,
}
