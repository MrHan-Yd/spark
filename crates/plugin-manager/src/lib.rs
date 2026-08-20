//! Plugin discovery, manifest handling, install/lifecycle, and state.
//!
//! 一期：webview 插件清单解析 + 安装/卸载/启停/授权 + 目录迁移 + 关键字路由匹配。
//! native 插件的进程 spawn 仍是后续工作。

mod db;
mod error;
mod manifest;
mod state;

pub use error::PluginError;
pub use manifest::{
    FeatureType, PluginCommand, PluginFeature, PluginManifest, PluginRuntime, PluginWindow,
};
pub use state::PluginState;

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// 插件来源：标准目录（可卸载）或开发目录（不拷贝，仅本会话）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginSource {
    Standard,
    Dev,
}

#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    pub manifest: PluginManifest,
    pub root: PathBuf,
    pub source: PluginSource,
    pub enabled: bool,
    pub granted: Vec<String>,
}

/// 设置页/IPC 返回的插件信息（序列化形状即 UI DTO）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub api_version: u32,
    pub runtime: String,
    pub description: Option<String>,
    pub author: Option<String>,
    pub icon: Option<String>,
    pub homepage: Option<String>,
    pub permissions: Vec<String>,
    pub granted: Vec<String>,
    pub enabled: bool,
    pub source: String,
    pub features: Vec<PluginFeature>,
}

/// `host.plugin.open` 返回：UI 据此打开 WebView2 窗口。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginOpenInfo {
    pub id: String,
    /// 插件显示名（清单 name），供窗口标题栏渲染。
    pub name: String,
    /// index.html 绝对路径。
    pub main_abs: String,
    pub window: PluginWindow,
    pub permissions: Vec<String>,
    pub granted: Vec<String>,
    /// 自定义 preload.js 绝对路径（清单声明且文件存在时）。
    pub preload_abs: Option<String>,
    /// 插件根目录绝对路径（用于 file:// 资源相对解析）。
    pub root: String,
    /// 图标绝对路径（清单 icon 拼 root，文件存在时）；None 时 UI 降级到内置图标。
    pub icon_abs: Option<String>,
}

/// 关键字路由匹配结果。
#[derive(Debug, Clone)]
pub struct KeywordMatch {
    pub plugin_id: String,
    pub feature_index: usize,
    /// 去掉关键字前缀后的用户输入（保留原始大小写）。
    pub input: String,
    pub keyword: String,
}

#[derive(Debug, Default)]
pub struct PluginManager {
    plugins: Vec<LoadedPlugin>,
    /// 标准（可装卸）插件目录。默认 `<exe_dir>/plugins`，可由 config/设置覆盖。
    plugins_dir: PathBuf,
    /// 状态文件与插件私有数据所在目录（默认全局 data_dir；测试可注入）。
    data_dir: PathBuf,
    state: PluginState,
}

impl PluginManager {
    pub fn new(plugins_dir: PathBuf) -> Self {
        Self::with_dirs(plugins_dir, spark_core::data_dir())
    }

    /// 指定 plugins_dir 与 data_dir 构造（测试用：避免污染全局 data_dir）。
    pub fn with_dirs(plugins_dir: PathBuf, data_dir: PathBuf) -> Self {
        let state = PluginState::load_at(&data_dir.join("plugins-state.json"));
        Self {
            plugins: Vec::new(),
            plugins_dir,
            data_dir,
            state,
        }
    }

    fn state_path(&self) -> PathBuf {
        self.data_dir.join("plugins-state.json")
    }

    /// 清单 `icon` 相对路径 → 拼接插件 root 后的绝对路径；文件不存在或未声明则 None。
    /// list() 与 open() 共用，保证设置页与窗口标题栏拿到一致的图标路径。
    fn icon_abs_of(p: &LoadedPlugin) -> Option<String> {
        p.manifest
            .icon
            .as_ref()
            .map(|ic| p.root.join(ic))
            .filter(|path| path.is_file())
            .map(|path| path.to_string_lossy().into_owned())
    }

    pub fn plugins(&self) -> &[LoadedPlugin] {
        &self.plugins
    }

    pub fn plugins_dir(&self) -> &Path {
        &self.plugins_dir
    }

    /// 扫描标准插件目录（`plugins_dir`）。
    pub fn scan_standard(&mut self) -> Result<usize, PluginError> {
        self.scan_dir_with_source(&self.plugins_dir.clone(), PluginSource::Standard)
    }

    /// 扫描一个目录并以指定来源登记插件。
    pub fn scan_dir(&mut self, dir: &Path) -> Result<usize, PluginError> {
        self.scan_dir_with_source(dir, PluginSource::Standard)
    }

    /// 加载一个开发目录（不拷贝；仅本会话有效，不可卸载，只能 remove_dev）。
    pub fn load_dev_dir(&mut self, dir: &Path) -> Result<String, PluginError> {
        let manifest = PluginManifest::load(&dir.join("plugin.json"))?;
        let id = manifest.id.clone();
        self.plugins
            .retain(|p| !(p.manifest.id == id && p.source == PluginSource::Dev));
        let enabled = self.state.enabled_of(&id);
        let granted = self.state.granted_of(&id);
        self.plugins.push(LoadedPlugin {
            manifest,
            root: dir.to_path_buf(),
            source: PluginSource::Dev,
            enabled,
            granted,
        });
        info!(id = %id, dev_dir = %dir.display(), "loaded dev plugin");
        Ok(id)
    }

    fn scan_dir_with_source(
        &mut self,
        dir: &Path,
        source: PluginSource,
    ) -> Result<usize, PluginError> {
        if !dir.is_dir() {
            return Ok(0);
        }
        let mut n = 0;
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let manifest_path = path.join("plugin.json");
            if !manifest_path.is_file() {
                continue;
            }
            match PluginManifest::load(&manifest_path) {
                Ok(manifest) => {
                    let id = manifest.id.clone();
                    // 同来源同 id 去重。
                    if self
                        .plugins
                        .iter()
                        .any(|p| p.manifest.id == id && p.source == source)
                    {
                        continue;
                    }
                    let enabled = self.state.enabled_of(&id);
                    let granted = self.state.granted_of(&id);
                    info!(id = %id, path = %path.display(), "loaded plugin manifest");
                    self.plugins.push(LoadedPlugin {
                        manifest,
                        root: path,
                        source,
                        enabled,
                        granted,
                    });
                    n += 1;
                }
                Err(e) => warn!(?e, path = %manifest_path.display(), "skip plugin"),
            }
        }
        Ok(n)
    }

    /// 从本地目录导入安装：拷贝到 `<plugins_dir>/<id>/` 并登记。
    pub fn install_from_dir(&mut self, src: &Path) -> Result<String, PluginError> {
        let manifest = PluginManifest::load(&src.join("plugin.json"))?;
        let id = manifest.id.clone();
        let dest = self.plugins_dir.join(&id);
        if dest.exists() {
            return Err(PluginError::Manifest(format!(
                "plugin already installed: {id}"
            )));
        }
        fs::create_dir_all(&self.plugins_dir)?;
        copy_dir_recursive(src, &dest)?;
        let m2 = PluginManifest::load(&dest.join("plugin.json"))?;
        self.plugins
            .retain(|p| !(p.manifest.id == id && p.source == PluginSource::Standard));
        let enabled = self.state.enabled_of(&id);
        let granted = self.state.granted_of(&id);
        self.plugins.push(LoadedPlugin {
            manifest: m2,
            root: dest,
            source: PluginSource::Standard,
            enabled,
            granted,
        });
        info!(id = %id, "installed plugin");
        Ok(id)
    }

    /// 卸载标准来源插件（开发插件不可卸载）。
    pub fn uninstall(&mut self, id: &str) -> Result<(), PluginError> {
        let pos = self
            .plugins
            .iter()
            .position(|p| p.manifest.id == id && p.source == PluginSource::Standard)
            .ok_or_else(|| PluginError::Manifest(format!("plugin not found: {id}")))?;
        let root = self.plugins[pos].root.clone();
        fs::remove_dir_all(&root)?;
        self.plugins.remove(pos);
        self.state.enabled.remove(id);
        self.state.granted.remove(id);
        self.state.save_to(&self.state_path())?;
        info!(id, "uninstalled plugin");
        Ok(())
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<(), PluginError> {
        let p = self
            .plugins
            .iter_mut()
            .find(|p| p.manifest.id == id)
            .ok_or_else(|| PluginError::Manifest(format!("plugin not found: {id}")))?;
        p.enabled = enabled;
        self.state.enabled.insert(id.to_string(), enabled);
        self.state.save_to(&self.state_path())?;
        Ok(())
    }

    pub fn grant(&mut self, id: &str, perms: Vec<String>) -> Result<(), PluginError> {
        let p = self
            .plugins
            .iter_mut()
            .find(|p| p.manifest.id == id)
            .ok_or_else(|| PluginError::Manifest(format!("plugin not found: {id}")))?;
        p.granted = perms.clone();
        self.state.granted.insert(id.to_string(), perms);
        self.state.save_to(&self.state_path())?;
        Ok(())
    }

    pub fn list(&self) -> Vec<PluginInfo> {
        self.plugins
            .iter()
            .map(|p| PluginInfo {
                id: p.manifest.id.clone(),
                name: p.manifest.name.clone(),
                version: p.manifest.version.clone(),
                api_version: p.manifest.api_version,
                runtime: runtime_str(&p.manifest.runtime),
                description: p.manifest.description.clone(),
                author: p.manifest.author.clone(),
                icon: Self::icon_abs_of(p),
                homepage: p.manifest.homepage.clone(),
                permissions: p.manifest.permissions.clone(),
                granted: p.granted.clone(),
                enabled: p.enabled,
                source: match p.source {
                    PluginSource::Standard => "standard".into(),
                    PluginSource::Dev => "dev".into(),
                },
                features: p.manifest.features.clone(),
            })
            .collect()
    }

    pub fn open(&self, id: &str) -> Result<PluginOpenInfo, PluginError> {
        let p = self
            .plugins
            .iter()
            .find(|p| p.manifest.id == id && p.enabled)
            .ok_or_else(|| PluginError::Manifest(format!("plugin not found or disabled: {id}")))?;
        if !matches!(p.manifest.runtime, PluginRuntime::Webview) {
            return Err(PluginError::Manifest(format!(
                "plugin {id} is not a webview plugin"
            )));
        }
        let main_abs = p.root.join(&p.manifest.main);
        if !main_abs.is_file() {
            return Err(PluginError::Manifest(format!(
                "plugin main not found: {}",
                main_abs.display()
            )));
        }
        let preload_abs = p
            .manifest
            .preload
            .as_ref()
            .map(|pr| p.root.join(pr))
            .filter(|p| p.is_file())
            .map(|p| p.to_string_lossy().into_owned());
        Ok(PluginOpenInfo {
            id: id.to_string(),
            name: p.manifest.name.clone(),
            main_abs: main_abs.to_string_lossy().into_owned(),
            window: p.manifest.window.clone().unwrap_or_default(),
            permissions: p.manifest.permissions.clone(),
            granted: p.granted.clone(),
            preload_abs,
            root: p.root.to_string_lossy().into_owned(),
            icon_abs: Self::icon_abs_of(p),
        })
    }

    /// 更换插件目录；`migrate=true` 时把现有标准插件子目录迁到新目录。
    ///
    /// 迁移策略：**先拷后删 + 失败回滚**。拷贝阶段任一插件失败 → 清掉本次已拷贝的、
    /// 旧目录不动、`plugins_dir` 不变、返回错（调用方据此不写 config，状态一致）；
    /// 全部拷贝成功才切目录，随后 best-effort 删旧源（删失败只留孤儿副本，不丢数据）。
    /// 调用方负责把结果写回 config.plugins_dir 并保存。
    pub fn set_dir(&mut self, new_dir: &Path, migrate: bool) -> Result<(), PluginError> {
        fs::create_dir_all(new_dir)?;
        if migrate && self.plugins_dir != new_dir {
            let to_migrate: Vec<(PathBuf, String)> = self
                .plugins
                .iter()
                .filter(|p| p.source == PluginSource::Standard)
                .map(|p| (p.root.clone(), p.manifest.id.clone()))
                .collect();
            let mut copied: Vec<(PathBuf, PathBuf)> = Vec::new();
            for (root, id) in &to_migrate {
                let dest = new_dir.join(id);
                if dest.exists() {
                    continue;
                }
                if let Err(e) = copy_dir_recursive(root, &dest) {
                    // 回滚：删掉本次已拷贝的，旧目录与 plugins_dir 保持不变。
                    for (_, c) in &copied {
                        let _ = fs::remove_dir_all(c);
                    }
                    return Err(e);
                }
                copied.push((root.clone(), dest));
            }
            // 全部拷贝成功 → 切目录 → best-effort 删旧源。
            self.plugins_dir = new_dir.to_path_buf();
            for (root, _dest) in &copied {
                let _ = fs::remove_dir_all(root);
            }
        } else {
            self.plugins_dir = new_dir.to_path_buf();
        }
        // 重新扫描新目录（dev 插件保留）。
        self.plugins.retain(|p| p.source == PluginSource::Dev);
        self.scan_dir_with_source(&self.plugins_dir.clone(), PluginSource::Standard)?;
        info!(dir = %self.plugins_dir.display(), "plugins dir changed");
        Ok(())
    }

    /// 查询路由：在 enabled 的 webview 插件中匹配关键字前缀。
    pub fn find_keyword_match(&self, text: &str) -> Option<KeywordMatch> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }
        let lower = trimmed.to_ascii_lowercase();
        for p in &self.plugins {
            if !p.enabled || !matches!(p.manifest.runtime, PluginRuntime::Webview) {
                continue;
            }
            for (fi, f) in p.manifest.features.iter().enumerate() {
                if f.mode != "page" {
                    continue;
                }
                if let Some(kw) = f.keyword() {
                    if kw.is_empty() {
                        continue;
                    }
                    let kw_l = kw.to_ascii_lowercase();
                    if lower == kw_l {
                        return Some(KeywordMatch {
                            plugin_id: p.manifest.id.clone(),
                            feature_index: fi,
                            input: String::new(),
                            keyword: kw.to_string(),
                        });
                    }
                    let prefix = format!("{kw_l} ");
                    if let Some(_rest) = lower.strip_prefix(&prefix) {
                        // 关键字已校验为 ASCII，按字节偏移取原始大小写输入。
                        let input = trimmed[kw.len() + 1..].to_string();
                        return Some(KeywordMatch {
                            plugin_id: p.manifest.id.clone(),
                            feature_index: fi,
                            input,
                            keyword: kw.to_string(),
                        });
                    }
                }
            }
        }
        None
    }

    pub fn granted(&self, id: &str) -> Vec<String> {
        self.plugins
            .iter()
            .find(|p| p.manifest.id == id)
            .map(|p| p.granted.clone())
            .unwrap_or_default()
    }

    pub fn declared_permissions(&self, id: &str) -> Vec<String> {
        self.plugins
            .iter()
            .find(|p| p.manifest.id == id)
            .map(|p| p.manifest.permissions.clone())
            .unwrap_or_default()
    }

    /// 插件私有 db 调用（capability=db；默认开放，无需授权）。
    pub fn plugin_db(
        &self,
        plugin_id: &str,
        method: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, PluginError> {
        db::invoke(&self.data_dir, plugin_id, method, args).map_err(PluginError::Io)
    }
}

fn runtime_str(r: &PluginRuntime) -> String {
    match r {
        PluginRuntime::Native => "native".into(),
        PluginRuntime::Wasm => "wasm".into(),
        PluginRuntime::Webview => "webview".into(),
    }
}

/// 递归拷贝目录。
fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<(), PluginError> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ft.is_file() {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_webview_plugin(dir: &Path, id: &str, keyword: &str) {
        fs::create_dir_all(dir).unwrap();
        let json = format!(
            r#"{{
                "id": "{id}", "name": "T", "version": "0.1.0", "api_version": 2,
                "runtime": "webview", "main": "index.html",
                "features": [{{ "type": "keyword", "keyword": "{keyword}", "title": "T", "mode": "page" }}]
            }}"#
        );
        fs::write(dir.join("plugin.json"), json).unwrap();
        fs::write(dir.join("index.html"), "<html></html>").unwrap();
    }

    #[test]
    fn keyword_match_exact_and_prefix() {
        let tmp = std::env::temp_dir().join("spark_pm_kw");
        let _ = fs::remove_dir_all(&tmp);
        make_webview_plugin(&tmp.join("com.spark.tr"), "com.spark.tr", "tr");
        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        pm.load_dev_dir(&tmp.join("com.spark.tr")).unwrap();

        let m = pm.find_keyword_match("tr").unwrap();
        assert_eq!(m.plugin_id, "com.spark.tr");
        assert!(m.input.is_empty());

        let m2 = pm.find_keyword_match("TR Hello").unwrap();
        assert_eq!(m2.input, "Hello");
        assert_eq!(m2.keyword, "tr");
    }

    #[test]
    fn keyword_match_disabled_skipped() {
        let tmp = std::env::temp_dir().join("spark_pm_dis");
        let _ = fs::remove_dir_all(&tmp);
        make_webview_plugin(&tmp.join("com.spark.tr"), "com.spark.tr", "tr");
        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        pm.load_dev_dir(&tmp.join("com.spark.tr")).unwrap();
        pm.set_enabled("com.spark.tr", false).unwrap();
        assert!(pm.find_keyword_match("tr").is_none());
    }

    #[test]
    fn install_and_uninstall_flow() {
        let tmp = std::env::temp_dir().join("spark_pm_install");
        let _ = fs::remove_dir_all(&tmp);
        // 源插件
        make_webview_plugin(
            &tmp.join("src").join("com.spark.hello"),
            "com.spark.hello",
            "hi",
        );
        let plugins_dir = tmp.join("plugins");
        let mut pm = PluginManager::with_dirs(plugins_dir.clone(), tmp.join("data"));
        let id = pm
            .install_from_dir(&tmp.join("src").join("com.spark.hello"))
            .unwrap();
        assert_eq!(id, "com.spark.hello");
        assert!(plugins_dir
            .join("com.spark.hello")
            .join("plugin.json")
            .is_file());
        assert_eq!(pm.list().len(), 1);

        pm.uninstall("com.spark.hello").unwrap();
        assert!(!plugins_dir.join("com.spark.hello").exists());
        assert!(pm.list().is_empty());
    }

    #[test]
    fn set_enabled_persists_via_state() {
        let tmp = std::env::temp_dir().join("spark_pm_toggle");
        let _ = fs::remove_dir_all(&tmp);
        make_webview_plugin(&tmp.join("com.spark.x"), "com.spark.x", "x");
        let data_dir = tmp.join("data");
        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), data_dir.clone());
        pm.load_dev_dir(&tmp.join("com.spark.x")).unwrap();
        pm.set_enabled("com.spark.x", false).unwrap();
        // 从同一临时 data_dir 重载状态应反映禁用
        let fresh = PluginState::load_at(&data_dir.join("plugins-state.json"));
        assert!(!fresh.enabled_of("com.spark.x"));
    }

    #[test]
    fn open_returns_main_path() {
        let tmp = std::env::temp_dir().join("spark_pm_open");
        let _ = fs::remove_dir_all(&tmp);
        make_webview_plugin(&tmp.join("com.spark.o"), "com.spark.o", "o");
        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        pm.load_dev_dir(&tmp.join("com.spark.o")).unwrap();
        let info = pm.open("com.spark.o").unwrap();
        assert!(info.main_abs.ends_with("index.html"));
        assert_eq!(info.window.width, 480); // default
    }

    #[test]
    fn set_dir_migrate_copies_plugins_and_rescans() {
        let tmp = std::env::temp_dir().join("spark_pm_setdir");
        let _ = fs::remove_dir_all(&tmp);
        let old = tmp.join("old_plugins");
        make_webview_plugin(&old.join("com.spark.m"), "com.spark.m", "m");
        let new = tmp.join("new_plugins");

        let mut pm = PluginManager::with_dirs(old.clone(), tmp.join("data"));
        pm.scan_standard().unwrap();
        assert_eq!(pm.list().len(), 1);

        pm.set_dir(&new, true).unwrap();
        // 新目录应含迁移来的插件，旧目录源应被删。
        assert!(new.join("com.spark.m").join("plugin.json").is_file());
        assert!(!old.join("com.spark.m").exists());
        assert_eq!(pm.plugins_dir(), new.as_path());
        // 重扫后内存列表来自新目录。
        assert_eq!(pm.list().len(), 1);
        assert_eq!(pm.list()[0].id, "com.spark.m");
    }

    #[test]
    fn set_dir_without_migrate_keeps_old() {
        let tmp = std::env::temp_dir().join("spark_pm_setdir_nomig");
        let _ = fs::remove_dir_all(&tmp);
        let old = tmp.join("old_plugins");
        make_webview_plugin(&old.join("com.spark.k"), "com.spark.k", "k");
        let new = tmp.join("new_plugins");

        let mut pm = PluginManager::with_dirs(old.clone(), tmp.join("data"));
        pm.scan_standard().unwrap();
        pm.set_dir(&new, false).unwrap();
        // 不迁移：旧目录保留，新目录为空，列表为空。
        assert!(old.join("com.spark.k").exists());
        assert!(!new.join("com.spark.k").exists());
        assert!(pm.list().is_empty());
    }
}
