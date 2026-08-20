use crate::PluginError;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginRuntime {
    Native,
    Wasm,
    Webview,
}

/// 触发入口类型（见《插件开发规范》§5）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeatureType {
    Keyword,
    Regex,
    Root,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginFeature {
    /// 触发类型；`keyword` 一期主推。
    #[serde(rename = "type")]
    pub kind: FeatureType,
    /// `type=keyword` 时必填：触发关键字，ASCII、无空格、1-4 字符推荐。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyword: Option<String>,
    /// `type=regex` 时必填：正则表达式（二期）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    /// `page`（开窗，webview）或 `list`（返回结果项，native）。
    #[serde(default = "default_feature_mode")]
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
}

fn default_feature_mode() -> String {
    "page".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginWindow {
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default = "default_height")]
    pub height: u32,
    #[serde(default = "default_min_width")]
    pub min_width: u32,
    #[serde(default = "default_min_height")]
    pub min_height: u32,
    #[serde(default = "default_true")]
    pub resizable: bool,
    #[serde(default)]
    pub always_on_top: bool,
    #[serde(default = "default_true")]
    pub frame: bool,
}

fn default_width() -> u32 {
    480
}
fn default_height() -> u32 {
    360
}
fn default_min_width() -> u32 {
    240
}
fn default_min_height() -> u32 {
    180
}
fn default_true() -> bool {
    true
}

impl Default for PluginWindow {
    fn default() -> Self {
        Self {
            width: 480,
            height: 360,
            min_width: 240,
            min_height: 180,
            resizable: true,
            always_on_top: false,
            frame: true,
        }
    }
}

/// native 插件的旧命令描述（向后兼容；webview 插件用 `features`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginCommand {
    pub name: String,
    pub title: String,
    #[serde(default)]
    pub subtitle: Option<String>,
    #[serde(default = "default_command_mode")]
    pub mode: String,
    #[serde(default)]
    pub prefix: Option<String>,
}

fn default_command_mode() -> String {
    "list".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub api_version: u32,
    pub main: String,
    pub runtime: PluginRuntime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// webview 插件可选的自定义预加载脚本相对路径。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preload: Option<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    /// native 插件命令（向后兼容）。
    #[serde(default)]
    pub commands: Vec<PluginCommand>,
    /// webview 插件触发入口（一期主推）。
    #[serde(default)]
    pub features: Vec<PluginFeature>,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<PluginWindow>,
}

impl PluginManifest {
    pub fn load(path: &Path) -> Result<Self, PluginError> {
        let raw = fs::read_to_string(path)?;
        let m: Self = serde_json::from_str(&raw)?;
        m.validate()?;
        Ok(m)
    }

    pub fn validate(&self) -> Result<(), PluginError> {
        if self.id.is_empty() || !self.id.contains('.') {
            return Err(PluginError::Manifest(
                "id must be reverse-domain style".into(),
            ));
        }
        if self.main.is_empty() {
            return Err(PluginError::Manifest("main is required".into()));
        }
        // 入口至少有一种：native 用 commands，webview 用 features。
        if self.commands.is_empty() && self.features.is_empty() {
            return Err(PluginError::Manifest(
                "either commands or features must be present".into(),
            ));
        }
        match self.runtime {
            PluginRuntime::Webview => {
                if !self.main.to_ascii_lowercase().ends_with(".html") {
                    return Err(PluginError::Manifest(
                        "webview plugin main must be an .html file".into(),
                    ));
                }
                if self.features.is_empty() {
                    return Err(PluginError::Manifest(
                        "webview plugin requires at least one feature".into(),
                    ));
                }
                for f in &self.features {
                    f.validate()?;
                }
            }
            PluginRuntime::Native => {
                if self.commands.is_empty() {
                    return Err(PluginError::Manifest(
                        "native plugin requires at least one command".into(),
                    ));
                }
            }
            PluginRuntime::Wasm => {}
        }
        Ok(())
    }
}

impl PluginFeature {
    pub fn validate(&self) -> Result<(), PluginError> {
        if self.mode != "page" && self.mode != "list" {
            return Err(PluginError::Manifest(format!(
                "feature mode must be 'page' or 'list', got '{}'",
                self.mode
            )));
        }
        match self.kind {
            FeatureType::Keyword => {
                let kw = self.keyword.as_deref().ok_or_else(|| {
                    PluginError::Manifest("keyword feature requires 'keyword'".into())
                })?;
                if kw.is_empty() {
                    return Err(PluginError::Manifest("keyword must not be empty".into()));
                }
                if !kw.is_ascii() {
                    return Err(PluginError::Manifest("keyword must be ASCII".into()));
                }
                if kw.contains(' ') {
                    return Err(PluginError::Manifest(
                        "keyword must not contain spaces".into(),
                    ));
                }
            }
            FeatureType::Regex => {
                if self
                    .pattern
                    .as_deref()
                    .map(|p| p.is_empty())
                    .unwrap_or(true)
                {
                    return Err(PluginError::Manifest(
                        "regex feature requires 'pattern'".into(),
                    ));
                }
            }
            FeatureType::Root => {}
        }
        Ok(())
    }

    /// 供查询路由用：返回该 feature 的关键字（仅 keyword 类型）。
    pub fn keyword(&self) -> Option<&str> {
        if self.kind == FeatureType::Keyword {
            self.keyword.as_deref()
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn webview_manifest_json(keyword: &str) -> String {
        format!(
            r#"{{
                "id": "com.spark.test",
                "name": "Test",
                "version": "0.1.0",
                "api_version": 2,
                "runtime": "webview",
                "main": "index.html",
                "features": [
                    {{ "type": "keyword", "keyword": "{keyword}", "title": "T", "mode": "page" }}
                ]
            }}"#
        )
    }

    #[test]
    fn webview_manifest_parses() {
        let m: PluginManifest = serde_json::from_str(&webview_manifest_json("tr")).unwrap();
        m.validate().unwrap();
        assert_eq!(m.runtime, PluginRuntime::Webview);
        assert_eq!(m.features.len(), 1);
        assert_eq!(m.features[0].keyword(), Some("tr"));
    }

    #[test]
    fn webview_rejects_non_html_main() {
        let json = webview_manifest_json("tr").replace("index.html", "app.exe");
        let m: PluginManifest = serde_json::from_str(&json).unwrap();
        assert!(m.validate().is_err());
    }

    #[test]
    fn keyword_with_space_rejected() {
        let m: PluginManifest = serde_json::from_str(&webview_manifest_json("a b")).unwrap();
        assert!(m.validate().is_err());
    }

    #[test]
    fn keyword_non_ascii_rejected() {
        let m: PluginManifest = serde_json::from_str(&webview_manifest_json("翻")).unwrap();
        assert!(m.validate().is_err());
    }

    #[test]
    fn webview_without_features_rejected() {
        let json = r#"{
            "id":"com.spark.x","name":"X","version":"0.1.0","api_version":2,
            "runtime":"webview","main":"index.html","features":[]
        }"#;
        let m: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(m.validate().is_err());
    }

    #[test]
    fn native_manifest_still_valid() {
        let json = r#"{
            "id":"com.spark.echo","name":"Echo","version":"0.1.0","api_version":1,
            "runtime":"native","main":"echo.exe",
            "commands":[{"name":"echo","title":"Echo","mode":"list","prefix":"echo "}]
        }"#;
        let m: PluginManifest = serde_json::from_str(json).unwrap();
        m.validate().unwrap();
        assert_eq!(m.runtime, PluginRuntime::Native);
    }
}
