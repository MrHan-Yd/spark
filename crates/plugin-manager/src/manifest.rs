use crate::PluginError;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// 插件清单规范版本（《插件开发规范》§11）。
///
/// 注意：这是**插件清单 `api_version`**，区别于 `spark_ipc::protocol::API_VERSION`
/// （那是 host↔UI 的 IPC 线协议版本，当前 = 1）。host 向后兼容：仅拒绝比本版本更新的
/// 清单（preload API 只增不删，旧清单在新 host 上仍可运行）。
pub const SUPPORTED_PLUGIN_API_VERSION: u32 = 2;

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
    /// `type=keyword` 时必填：触发关键字，无空格、1-4 字符推荐；支持中文。
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
        if self.api_version > SUPPORTED_PLUGIN_API_VERSION {
            return Err(PluginError::Manifest(format!(
                "api_version {} 不受支持（host 支持 <= {}）",
                self.api_version, SUPPORTED_PLUGIN_API_VERSION
            )));
        }
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

/// 宽松版本号比较：与 host 自更新 C# 端 `ParseVersion` 行为一致。
///
/// 规则：跳过前导非数字（兼容 `v0.1.0`）→ 按 `.` 分段取前 3 段为 `u32`
/// （缺失/解析失败补 0）→ 元组比较。双方都无法解析出任何数字段时回退字符串比较。
/// 用于插件覆盖安装时判定升级/平级/降级。
pub fn cmp_version(a: &str, b: &str) -> std::cmp::Ordering {
    let av = parse_version_tuple(a);
    let bv = parse_version_tuple(b);
    // 双方都未解析出数字段（返回 None）→ 回退字符串比较，保证稳定全序。
    match (av, bv) {
        (Some(av), Some(bv)) => av.cmp(&bv),
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (None, None) => a.cmp(b),
    }
}

/// 解析版本号前 3 段为 `(u32, u32, u32)` 元组；若完全无数字段返回 None。
fn parse_version_tuple(s: &str) -> Option<(u32, u32, u32)> {
    // 跳过前导非数字（处理 "v0.1.0" 等前缀）；整串无数字则返回 None 走字符串回退。
    let digits = s.trim_start_matches(|c: char| !c.is_ascii_digit());
    if digits.is_empty() {
        return None;
    }
    let mut parts = digits.split('.').map(|seg| {
        // 每段取连续数字部分解析，非数字尾随（如 "0beta"）取前缀 "0"；空段补 0。
        let num: String = seg.chars().take_while(|c| c.is_ascii_digit()).collect();
        num.parse::<u32>().unwrap_or(0)
    });
    let major = parts.next()?;
    let minor = parts.next().unwrap_or(0);
    let patch = parts.next().unwrap_or(0);
    Some((major, minor, patch))
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
                // 路由以 ASCII 空格为参数分隔符；关键字含任意空白（含全角空格）都会产生歧义。
                if kw.chars().any(char::is_whitespace) {
                    return Err(PluginError::Manifest(
                        "keyword must not contain whitespace".into(),
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
    fn keyword_chinese_accepted() {
        // 规范只要求无空格；中文关键字（如 "翻译"）合法。
        let m: PluginManifest = serde_json::from_str(&webview_manifest_json("翻译")).unwrap();
        m.validate().unwrap();
        assert_eq!(m.features[0].keyword(), Some("翻译"));
    }

    #[test]
    fn keyword_full_width_space_rejected() {
        // 全角空格同样是空格分隔符，必须拒绝。
        let m: PluginManifest = serde_json::from_str(&webview_manifest_json("翻　译")).unwrap();
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

    #[test]
    fn api_version_too_new_rejected() {
        // 比当前支持版本更新的清单不加载（向后兼容旧版本）。
        let json = webview_manifest_json("tr").replace("\"api_version\": 2", "\"api_version\": 99");
        let m: PluginManifest = serde_json::from_str(&json).unwrap();
        assert!(m.validate().is_err());
    }

    #[test]
    fn api_version_old_still_accepted() {
        // 旧规范版本（如 native 时代的 1）在新 host 上仍可加载。
        let json = r#"{
            "id":"com.spark.old","name":"Old","version":"0.1.0","api_version":1,
            "runtime":"native","main":"old.exe",
            "commands":[{"name":"old","title":"Old","mode":"list","prefix":"old "}]
        }"#;
        let m: PluginManifest = serde_json::from_str(json).unwrap();
        m.validate().unwrap();
    }

    #[test]
    fn cmp_version_ordering() {
        use std::cmp::Ordering;
        // 升级
        assert_eq!(cmp_version("0.1.0", "0.2.0"), Ordering::Less);
        assert_eq!(cmp_version("0.2.0", "0.1.0"), Ordering::Greater);
        // 平级
        assert_eq!(cmp_version("1.0.0", "1.0.0"), Ordering::Equal);
        // 缺段补 0
        assert_eq!(cmp_version("1.0", "1.0.0"), Ordering::Equal);
        assert_eq!(cmp_version("2.0", "2.0.1"), Ordering::Less);
        // v 前缀
        assert_eq!(cmp_version("v0.1.0", "0.1.0"), Ordering::Equal);
        assert_eq!(cmp_version("v1.2.3", "v1.2.4"), Ordering::Less);
        // 尾随非数字
        assert_eq!(cmp_version("0.1.0beta", "0.1.0"), Ordering::Equal);
    }

    #[test]
    fn cmp_version_fallback_string() {
        use std::cmp::Ordering;
        // 双方都无数字段 → 字符串比较，保证全序稳定。
        assert_eq!(cmp_version("alpha", "beta"), Ordering::Less);
        assert_eq!(cmp_version("beta", "alpha"), Ordering::Greater);
        // 一方有数字段优先于纯文本。
        assert_eq!(cmp_version("0.0.1", "nope"), Ordering::Greater);
        assert_eq!(cmp_version("nope", "0.0.1"), Ordering::Less);
    }
}
