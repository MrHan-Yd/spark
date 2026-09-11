use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    App,
    File,
    History,
    Favorite,
    Plugin,
    Builtin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IconRef {
    Path { path: String },
    Glyph { name: String },
    Plugin { plugin_id: String, name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub id: String,
    pub title: String,
    pub is_default: bool,
    /// Optional per-action launch target (e.g. a merged secondary shortcut);
    /// falls back to the candidate's own target when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

impl Action {
    pub fn open_default() -> Self {
        Self {
            id: "open".into(),
            title: "打开".into(),
            is_default: true,
            target: None,
        }
    }

    pub fn reveal() -> Self {
        Self {
            id: "reveal".into(),
            title: "打开文件位置".into(),
            is_default: false,
            target: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub id: String,
    pub title: String,
    pub subtitle: Option<String>,
    /// Launch target: .lnk / .exe / protocol path
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Optional icon path (shortcut target or .ico)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub score: f32,
    pub source: Source,
    pub actions: Vec<Action>,
    pub plugin_id: Option<String>,
    /// 插件候选的触发上下文（仅 `source=plugin` 时由 host 填写，匹配即权威）：
    /// keyword 命中 = `(关键字, 去前缀剩余)`；regex/root 命中 = `(空, 完整查询)`。
    /// UI 开窗直接取用，不再自行拆分输入——两侧对同一次触发算出一致的
    /// `spark.input.*`（regex/root 候选没有关键字前缀，UI 首词拆分会拆错）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_input: Option<String>,
}

impl Candidate {
    pub fn app(id: impl Into<String>, title: impl Into<String>, target: impl Into<String>) -> Self {
        let target = target.into();
        Self {
            id: id.into(),
            title: title.into(),
            subtitle: Some("应用程序".into()),
            icon: Some(target.clone()),
            target: Some(target),
            score: 1.0,
            source: Source::App,
            actions: vec![Action::open_default(), Action::reveal()],
            plugin_id: None,
            plugin_command: None,
            plugin_input: None,
        }
    }
}
