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
}

impl Action {
    pub fn open_default() -> Self {
        Self {
            id: "open".into(),
            title: "打开".into(),
            is_default: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub score: f32,
    pub source: Source,
    pub actions: Vec<Action>,
    pub plugin_id: Option<String>,
}
