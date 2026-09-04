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
    /// `page`（开窗）；`list` 已移除（旧 native"exe 直接应答搜索"模式）。
    /// webview 全量允许；native 仅接受 `page`。
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
    /// 旧版 native 关键字（已废弃：native 纯应用模型下校验直接拒绝）。
    #[serde(default)]
    pub keywords: Vec<String>,
    /// 旧版 native 命令（已废弃：native 纯应用模型下校验直接拒绝）。
    #[serde(default)]
    pub commands: Vec<PluginCommand>,
    /// 插件触发入口：webview 全量；native 仅 page 模式（关键字只做"搜索框 →
    /// 打开页面"入口，exe 永不直接产出搜索结果）。
    #[serde(default)]
    pub features: Vec<PluginFeature>,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<PluginWindow>,
    /// native 插件页面入口（相对插件根目录的 HTML 路径）；native 必填。
    /// native 是"纯应用"模型：页面是插件的全部 UI，从插件卡片「打开」或搜索框
    /// 关键字（features page 模式）进入；exe 只经 `plugin.page` RPC 服务于页面。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
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
        // 入口至少有一种：webview 用 features；native 必有 page（可另声明
        // features 作搜索入口）。
        if self.features.is_empty() && self.page.is_none() {
            return Err(PluginError::Manifest(
                "either features or page must be present".into(),
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
                // native 纯应用模型：旧 commands/keywords（exe 直接向搜索框应答的
                // list 模式）仍拒绝；features 与 webview 同构，但仅限 page 模式——
                // 关键字只做"搜索框 → 打开页面"入口，exe 永不直接产出搜索结果。
                if !self.commands.is_empty() {
                    return Err(PluginError::Manifest(
                        "native plugin no longer supports 'commands' (use 'features' with mode=page)"
                            .into(),
                    ));
                }
                if !self.keywords.is_empty() {
                    return Err(PluginError::Manifest(
                        "native plugin no longer supports 'keywords' (use 'features')".into(),
                    ));
                }
                for f in &self.features {
                    f.validate()?;
                    if f.mode != "page" {
                        return Err(PluginError::Manifest(format!(
                            "native feature mode must be 'page', got '{}'",
                            f.mode
                        )));
                    }
                }
                let page = self.page.as_deref().ok_or_else(|| {
                    PluginError::Manifest("native plugin requires 'page' (HTML entry)".into())
                })?;
                validate_page_path(page)?;
            }
            PluginRuntime::Wasm => {}
        }
        Ok(())
    }
}

/// native `page` 路径校验：仅允许"普通相对路径"——所有组件必须是 `Normal`
/// （拒绝绝对路径/盘符前缀/根路径/`..` 上跳/`.` 当前目录）。页面由 WebView2 以
/// 虚拟主机映射加载，路径越界会读到插件目录之外的内容；`C:evil`（有前缀无根）、
/// `\evil`（有根无前缀）这类 `is_absolute()` 拦不住的形状也被 Normal 约束覆盖。
fn validate_page_path(page: &str) -> Result<(), PluginError> {
    if page.is_empty() {
        return Err(PluginError::Manifest("page must not be empty".into()));
    }
    let all_normal = Path::new(page)
        .components()
        .all(|c| matches!(c, std::path::Component::Normal(_)));
    if !all_normal {
        return Err(PluginError::Manifest(
            "page must be a plain relative path (no '..' / drive / rooted components)".into(),
        ));
    }
    if !page.to_ascii_lowercase().ends_with(".html") {
        return Err(PluginError::Manifest("page must be an .html file".into()));
    }
    Ok(())
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
            "id":"com.spark.echo","name":"Echo","version":"0.1.0","api_version":2,
            "runtime":"native","main":"echo.exe","page":"page.html"
        }"#;
        let m: PluginManifest = serde_json::from_str(json).unwrap();
        m.validate().unwrap();
        assert_eq!(m.runtime, PluginRuntime::Native);
        assert_eq!(m.page.as_deref(), Some("page.html"));
    }

    #[test]
    fn native_requires_page() {
        let json = r#"{
            "id":"com.spark.x","name":"X","version":"0.1.0","api_version":2,
            "runtime":"native","main":"x.exe"
        }"#;
        let m: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(m.validate().is_err());
    }

    #[test]
    fn native_rejects_commands() {
        // 纯应用模型：native 不得声明 commands（搜索框入口），否则拒绝加载。
        let json = r#"{
            "id":"com.spark.x","name":"X","version":"0.1.0","api_version":2,
            "runtime":"native","main":"x.exe","page":"page.html",
            "commands":[{"name":"x","title":"X","mode":"list","prefix":"x "}]
        }"#;
        let m: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(m.validate().is_err());
    }

    #[test]
    fn native_rejects_keywords() {
        let json = r#"{
            "id":"com.spark.x","name":"X","version":"0.1.0","api_version":2,
            "runtime":"native","main":"x.exe","page":"page.html","keywords":["x"]
        }"#;
        let m: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(m.validate().is_err());
    }

    #[test]
    fn native_accepts_features_page_mode() {
        // 纯应用模型保留 features 关键字入口：搜索框搜到 → 打开页面（同 webview UX）。
        let json = r#"{
            "id":"com.spark.x","name":"X","version":"0.1.0","api_version":2,
            "runtime":"native","main":"x.exe","page":"page.html",
            "features":[{"type":"keyword","keyword":"x","title":"X","mode":"page"}]
        }"#;
        let m: PluginManifest = serde_json::from_str(json).unwrap();
        m.validate().unwrap();
        assert_eq!(m.features[0].keyword(), Some("x"));
    }

    #[test]
    fn native_rejects_features_list_mode() {
        // native feature 只许 page 模式：list 意味着 exe 直接产出搜索结果，
        // 正是纯应用模型移除的能力。
        let json = r#"{
            "id":"com.spark.x","name":"X","version":"0.1.0","api_version":2,
            "runtime":"native","main":"x.exe","page":"page.html",
            "features":[{"type":"keyword","keyword":"x","title":"X","mode":"list"}]
        }"#;
        let m: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(m.validate().is_err());
    }

    #[test]
    fn native_page_path_rejects_absolute() {
        let json = r#"{
            "id":"com.spark.x","name":"X","version":"0.1.0","api_version":2,
            "runtime":"native","main":"x.exe","page":"C:\\evil\\page.html"
        }"#;
        let m: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(m.validate().is_err());
    }

    #[test]
    fn native_page_path_rejects_drive_prefix_and_rooted() {
        // is_absolute() 拦不住的形状：盘符相对路径（C:evil）与根相对（/evil），
        // 都被 all-Normal 约束拒绝。
        for page in ["C:evil/page.html", "/evil/page.html", "../page.html"] {
            let json = format!(
                r#"{{
                    "id":"com.spark.x","name":"X","version":"0.1.0","api_version":2,
                    "runtime":"native","main":"x.exe","page":"{page}"
                }}"#
            );
            let m: PluginManifest = serde_json::from_str(&json).unwrap();
            assert!(m.validate().is_err(), "page {page:?} must be rejected");
        }
        // 纯 "." 组件（./page.html）同样被拒（Normal 约束）。
        let json = r#"{
            "id":"com.spark.x","name":"X","version":"0.1.0","api_version":2,
            "runtime":"native","main":"x.exe","page":"./page.html"
        }"#;
        let m: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(m.validate().is_err());
    }

    #[test]
    fn native_page_path_rejects_parent_dir() {
        let json = r#"{
            "id":"com.spark.x","name":"X","version":"0.1.0","api_version":2,
            "runtime":"native","main":"x.exe","page":"..\\evil\\page.html"
        }"#;
        let m: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(m.validate().is_err());
    }

    #[test]
    fn native_page_path_rejects_non_html() {
        let json = r#"{
            "id":"com.spark.x","name":"X","version":"0.1.0","api_version":2,
            "runtime":"native","main":"x.exe","page":"assets/page.exe"
        }"#;
        let m: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(m.validate().is_err());
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
            "runtime":"native","main":"old.exe","page":"page.html"
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
