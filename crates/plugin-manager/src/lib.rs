//! Plugin discovery, manifest handling, install/lifecycle, and state.
//!
//! 一期：webview 插件清单解析 + 安装/卸载/启停/授权 + 目录迁移 + 关键字路由匹配。
//! native 插件的进程 spawn 仍是后续工作。

mod db;
mod error;
mod manifest;
mod native;
mod state;

pub use error::PluginError;
pub use manifest::{
    cmp_version, FeatureType, PluginCommand, PluginFeature, PluginManifest, PluginRuntime,
    PluginWindow,
};
pub use native::{NativeMatch, NativeRuntime, NativeSpawnInfo};
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

/// `install_from_dir` 的执行结果：告知 UI 是新装、覆盖更新还是需要确认降级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallAction {
    /// 全新安装（目录原先不存在）。
    Installed,
    /// 覆盖更新（新版 >= 旧版，或 force=true 强制覆盖）。
    Updated,
    /// 检测到旧版本（新版 < 旧版），未写盘，等 UI 弹窗确认后以 force=true 重试。
    ConfirmDowngrade,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInstallOutcome {
    pub id: String,
    pub action: InstallAction,
    /// 新装版本号（源 manifest 的 version）。
    pub version: String,
    /// 旧版本号；全新装为 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_version: Option<String>,
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

/// 关键字冲突：同一关键字被多个已启用的 webview 插件占用。
/// 路由仍按加载顺序取首个生效；本结构仅用于在设置页/日志暴露冲突，供开发者排查。
#[derive(Debug, Clone)]
pub struct KeywordConflict {
    /// 小写后的关键字。
    pub keyword: String,
    /// 占用该关键字的插件 id（按加载顺序）。
    pub plugin_ids: Vec<String>,
}

/// 不 derive Debug：`NativeRuntime` 持有子进程句柄（Child/管道），不可 Debug。
/// Default 仍可用：所有字段均有 Default（NativeRuntime::default() 为空进程表）。
#[derive(Default)]
pub struct PluginManager {
    plugins: Vec<LoadedPlugin>,
    /// 标准（可装卸）插件目录。默认 `<exe_dir>/plugins`，可由 config/设置覆盖。
    plugins_dir: PathBuf,
    /// 状态文件与插件私有数据所在目录（默认全局 data_dir；测试可注入）。
    data_dir: PathBuf,
    state: PluginState,
    /// native 插件进程运行时（懒启动 + 常驻 + 崩溃重建）。
    native: NativeRuntime,
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
            native: NativeRuntime::default(),
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
        // 启动时 best-effort 清理上次覆盖安装崩溃残留的暂存/备份目录（点前缀）。
        self.cleanup_staging_residue();
        self.scan_dir_with_source(&self.plugins_dir.clone(), PluginSource::Standard)
    }

    /// 清理 plugins_dir 下的 `.*.staging` / `.*.bak` 残留（崩溃中断遗留）。
    /// best-effort：失败仅 warn，不影响扫描。
    fn cleanup_staging_residue(&self) {
        let Ok(entries) = fs::read_dir(&self.plugins_dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !name.starts_with('.') {
                continue;
            }
            if name.ends_with(".staging") || name.ends_with(".bak") {
                if let Err(e) = fs::remove_dir_all(entry.path()) {
                    warn!(path = %entry.path().display(), ?e, "skip leftover staging/bak");
                } else {
                    info!(path = %entry.path().display(), "removed leftover staging/bak");
                }
            }
        }
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
        self.warn_keyword_conflicts();
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
            // 跳过点前缀目录：覆盖更新用的 .staging/.bak 残留绝不当作插件加载
            // （插件 id 是反域名格式，目录名不以 '.' 开头）。
            if entry
                .file_name()
                .to_str()
                .map(|n| n.starts_with('.'))
                .unwrap_or(true)
            {
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
        self.warn_keyword_conflicts();
        Ok(n)
    }

    /// 从本地目录导入安装：拷贝到 `<plugins_dir>/<id>/` 并登记。
    ///
    /// 若插件 id 已存在则按版本号决定行为（本地覆盖更新机制）：
    /// - 新版 >= 旧版（或 `force=true`）：关 native 进程 → 暂存拷贝 + 备份交换
    ///   （先拷到 `.<id>.staging`，成功后旧目录改名 `.<id>.bak` 再换入目标，任一步失败
    ///   均回滚保留旧插件完整可用）→ 保留 enabled/granted（granted 裁剪为与新声明
    ///   权限的交集并持久化），返回 `Updated`。
    /// - 新版 < 旧版且 `!force`：**不写盘**，返回 `ConfirmDowngrade`，由 UI 弹窗确认后
    ///   以 `force=true` 重试覆盖。
    pub fn install_from_dir(
        &mut self,
        src: &Path,
        force: bool,
    ) -> Result<PluginInstallOutcome, PluginError> {
        let manifest = PluginManifest::load(&src.join("plugin.json"))?;
        let id = manifest.id.clone();
        let new_version = manifest.version.clone();
        let dest = self.plugins_dir.join(&id);

        if !dest.exists() {
            // 全新安装。
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
            self.warn_keyword_conflicts();
            return Ok(PluginInstallOutcome {
                id,
                action: InstallAction::Installed,
                version: new_version,
                previous_version: None,
            });
        }

        // 已存在：比对版本决定覆盖/确认降级。
        let old = PluginManifest::load(&dest.join("plugin.json"))?;
        let old_version = old.version.clone();
        let is_downgrade = cmp_version(&new_version, &old_version) == std::cmp::Ordering::Less;
        if is_downgrade && !force {
            // 不写盘：纯探查，等 UI 确认后以 force=true 重试。
            info!(
                id = %id,
                new = %new_version, old = %old_version,
                "plugin downgrade detected, awaiting confirmation"
            );
            return Ok(PluginInstallOutcome {
                id,
                action: InstallAction::ConfirmDowngrade,
                version: new_version,
                previous_version: Some(old_version),
            });
        }

        // 覆盖更新：暂存拷贝 + 备份交换，保证拷贝失败不破坏现有插件
        // （与 set_dir 迁移的"先拷后删+失败回滚"安全模式一致）。
        self.native.shutdown_plugin(&id);
        let staging = self.plugins_dir.join(format!(".{id}.staging"));
        let backup = self.plugins_dir.join(format!(".{id}.bak"));
        // 清理上次崩溃可能残留的暂存/备份目录（best-effort）。
        let _ = fs::remove_dir_all(&staging);
        let _ = fs::remove_dir_all(&backup);

        // 1) 拷贝源到暂存目录；失败则旧目录与内存条目均不变，清暂存。
        if let Err(e) = copy_dir_recursive(src, &staging) {
            let _ = fs::remove_dir_all(&staging);
            return Err(e);
        }
        // 2) 旧目录改名到备份；失败（如被 webview 窗口占用文件句柄）则保留旧目录，清暂存。
        if let Err(e) = fs::rename(&dest, &backup) {
            let _ = fs::remove_dir_all(&staging);
            return Err(PluginError::Io(e));
        }
        // 3) 暂存改名到目标；失败则把备份还原回目标，清暂存（旧插件完整可用）。
        if let Err(e) = fs::rename(&staging, &dest) {
            if let Err(re) = fs::rename(&backup, &dest) {
                warn!(id = %id, ?re, "backup restore failed, old plugin may need manual recovery");
            }
            // 清理暂存（与 step 1/2 对称），须在备份还原之后。
            let _ = fs::remove_dir_all(&staging);
            return Err(PluginError::Io(e));
        }
        // 4) 成功：删备份（best-effort）。
        let _ = fs::remove_dir_all(&backup);

        let m2 = PluginManifest::load(&dest.join("plugin.json"))?;
        self.plugins
            .retain(|p| !(p.manifest.id == id && p.source == PluginSource::Standard));
        // 保留用户已设的启停/授权状态；granted 裁剪为与新声明权限的交集
        // （新版本可能删减了某些权限声明，陈旧授权项随之清理）并持久化，避免重启后回读脏数据。
        let enabled = self.state.enabled_of(&id);
        let granted: Vec<String> = self
            .state
            .granted_of(&id)
            .into_iter()
            .filter(|g| m2.permissions.iter().any(|p| p == g))
            .collect();
        self.state.granted.insert(id.clone(), granted.clone());
        self.state.save_to(&self.state_path())?;
        self.plugins.push(LoadedPlugin {
            manifest: m2,
            root: dest,
            source: PluginSource::Standard,
            enabled,
            granted,
        });
        info!(id = %id, new = %new_version, old = %old_version, "updated plugin");
        self.warn_keyword_conflicts();
        Ok(PluginInstallOutcome {
            id,
            action: InstallAction::Updated,
            version: new_version,
            previous_version: Some(old_version),
        })
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

    /// 检测已启用 webview 插件间的 page 关键字冲突（同一关键字被多个插件占用）。
    ///
    /// 仅统计 `enabled` 的 webview 插件：禁用的插件不参与路由，不构成实际冲突。
    /// 路由仍按加载顺序首匹配胜出（`find_keyword_match`）；本方法只暴露冲突供日志/设置页提示。
    pub fn keyword_conflicts(&self) -> Vec<KeywordConflict> {
        use std::collections::BTreeMap;
        let mut by_kw: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for p in &self.plugins {
            if !p.enabled || !matches!(p.manifest.runtime, PluginRuntime::Webview) {
                continue;
            }
            // 统计粒度落在“插件”而非“feature”：同一插件内多个 feature 用相同关键字
            // 不构成“插件间”冲突，故同一 plugin_id 在同一关键字下只计一次。
            for f in &p.manifest.features {
                if f.mode != "page" {
                    continue;
                }
                if let Some(kw) = f.keyword() {
                    if !kw.is_empty() {
                        let ids = by_kw.entry(kw.to_ascii_lowercase()).or_default();
                        if !ids.contains(&p.manifest.id) {
                            ids.push(p.manifest.id.clone());
                        }
                    }
                }
            }
        }
        by_kw
            .into_iter()
            .filter(|(_, ids)| ids.len() > 1)
            .map(|(kw, ids)| KeywordConflict {
                keyword: kw,
                plugin_ids: ids,
            })
            .collect()
    }

    /// 对当前已启用插件的关键字冲突打 warn 日志。在新增插件的入口处调用。
    fn warn_keyword_conflicts(&self) {
        for c in self.keyword_conflicts() {
            warn!(
                keyword = %c.keyword,
                plugins = ?c.plugin_ids,
                "关键字冲突：多个已启用插件占用同一关键字，仅加载顺序首个生效"
            );
        }
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

    // ─── native 插件运行时委托 ───────────────────────────────────────────────

    /// 构造某 native 插件的启动信息（owned）。调用方拿到后即可结束不可变借用，
    /// 再可变调用 `native_query`/`native_invoke`，避免借用冲突。
    pub fn native_spawn_info(&self, id: &str) -> Option<NativeSpawnInfo> {
        self.native_plugin(id).map(NativeSpawnInfo::from_plugin)
    }

    /// native 查询：懒启动进程 → query → 返回结果项。
    /// 进程崩溃/超时自动重建，本次失败降级为空结果（不抛错给搜索主流程）。
    pub fn native_query(
        &mut self,
        id: &str,
        text: &str,
        limit: u32,
    ) -> Result<spark_ipc::QueryResult, PluginError> {
        let info = match self.native_spawn_info(id) {
            Some(i) => i,
            None => {
                return Ok(spark_ipc::QueryResult {
                    items: vec![],
                    partial: false,
                });
            }
        };
        self.native.query(&info, text, limit)
    }

    /// native 执行：用户选中某结果项动作时调用。
    pub fn native_invoke(
        &mut self,
        id: &str,
        params: spark_ipc::InvokeParams,
    ) -> Result<spark_ipc::InvokeResult, PluginError> {
        let info = self
            .native_spawn_info(id)
            .ok_or_else(|| PluginError::Manifest(format!("native plugin not found: {id}")))?;
        self.native.invoke(&info, params)
    }

    /// host 退出前调用：向所有 native 进程发 shutdown 并 wait。
    pub fn native_shutdown_all(&mut self) {
        self.native.shutdown_all();
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
        make_webview_plugin_ver(dir, id, keyword, "0.1.0");
    }

    fn make_webview_plugin_ver(dir: &Path, id: &str, keyword: &str, version: &str) {
        fs::create_dir_all(dir).unwrap();
        let json = format!(
            r#"{{
                "id": "{id}", "name": "T", "version": "{version}", "api_version": 2,
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
        let outcome = pm
            .install_from_dir(&tmp.join("src").join("com.spark.hello"), false)
            .unwrap();
        assert_eq!(outcome.id, "com.spark.hello");
        assert_eq!(outcome.action, InstallAction::Installed);
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

    #[test]
    fn keyword_conflict_detected() {
        let tmp = std::env::temp_dir().join("spark_pm_conflict");
        let _ = fs::remove_dir_all(&tmp);
        make_webview_plugin(&tmp.join("com.spark.a"), "com.spark.a", "hi");
        make_webview_plugin(&tmp.join("com.spark.b"), "com.spark.b", "hi");
        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        pm.load_dev_dir(&tmp.join("com.spark.a")).unwrap();
        pm.load_dev_dir(&tmp.join("com.spark.b")).unwrap();

        // 两者默认启用 → 冲突；按加载顺序首个路由生效。
        let conflicts = pm.keyword_conflicts();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].keyword, "hi");
        assert_eq!(conflicts[0].plugin_ids, vec!["com.spark.a", "com.spark.b"]);
        assert_eq!(
            pm.find_keyword_match("hi").map(|m| m.plugin_id),
            Some("com.spark.a".to_string())
        );

        // 禁用其一 → 不再构成冲突。
        pm.set_enabled("com.spark.b", false).unwrap();
        assert!(pm.keyword_conflicts().is_empty());
    }

    #[test]
    fn keyword_conflict_not_triggered_by_single_plugin_duplicate() {
        // 同一插件清单内两个 feature 用相同关键字不算“插件间”冲突。
        let tmp = std::env::temp_dir().join("spark_pm_self_dup");
        let _ = fs::remove_dir_all(&tmp);
        let dir = tmp.join("com.spark.d");
        fs::create_dir_all(&dir).unwrap();
        let json = r#"{
            "id": "com.spark.d", "name": "D", "version": "0.1.0", "api_version": 2,
            "runtime": "webview", "main": "index.html",
            "features": [
                { "type": "keyword", "keyword": "go", "title": "A", "mode": "page" },
                { "type": "keyword", "keyword": "go", "title": "B", "mode": "page" }
            ]
        }"#;
        fs::write(dir.join("plugin.json"), json).unwrap();
        fs::write(dir.join("index.html"), "<html></html>").unwrap();
        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        pm.load_dev_dir(&dir).unwrap();
        assert!(pm.keyword_conflicts().is_empty());
    }

    #[test]
    fn install_overwrite_newer() {
        let tmp = std::env::temp_dir().join("spark_pm_up");
        let _ = fs::remove_dir_all(&tmp);
        // 先装 0.1.0
        make_webview_plugin_ver(
            &tmp.join("v1").join("com.spark.h"),
            "com.spark.h",
            "h",
            "0.1.0",
        );
        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        let o = pm
            .install_from_dir(&tmp.join("v1").join("com.spark.h"), false)
            .unwrap();
        assert_eq!(o.action, InstallAction::Installed);
        assert_eq!(o.version, "0.1.0");

        // 再装 0.2.0：新版 >= 旧版，静默覆盖
        make_webview_plugin_ver(
            &tmp.join("v2").join("com.spark.h"),
            "com.spark.h",
            "h",
            "0.2.0",
        );
        let o = pm
            .install_from_dir(&tmp.join("v2").join("com.spark.h"), false)
            .unwrap();
        assert_eq!(o.action, InstallAction::Updated);
        assert_eq!(o.version, "0.2.0");
        assert_eq!(o.previous_version.as_deref(), Some("0.1.0"));
        // 列表里的版本已更新
        assert_eq!(pm.list()[0].version, "0.2.0");
    }

    #[test]
    fn install_overwrite_leaves_no_staging_residue() {
        let tmp = std::env::temp_dir().join("spark_pm_residue");
        let _ = fs::remove_dir_all(&tmp);
        make_webview_plugin_ver(
            &tmp.join("v1").join("com.spark.h"),
            "com.spark.h",
            "h",
            "0.1.0",
        );
        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        pm.install_from_dir(&tmp.join("v1").join("com.spark.h"), false)
            .unwrap();
        make_webview_plugin_ver(
            &tmp.join("v2").join("com.spark.h"),
            "com.spark.h",
            "h",
            "0.2.0",
        );
        pm.install_from_dir(&tmp.join("v2").join("com.spark.h"), false)
            .unwrap();
        // 暂存/备份目录应已清理，不残留。
        let plugins = tmp.join("plugins");
        assert!(!plugins.join(".com.spark.h.staging").exists());
        assert!(!plugins.join(".com.spark.h.bak").exists());
        assert!(plugins.join("com.spark.h").join("plugin.json").is_file());
    }

    #[test]
    fn install_overwrite_same_version() {
        let tmp = std::env::temp_dir().join("spark_pm_same");
        let _ = fs::remove_dir_all(&tmp);
        make_webview_plugin_ver(
            &tmp.join("v1").join("com.spark.h"),
            "com.spark.h",
            "h",
            "0.1.0",
        );
        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        pm.install_from_dir(&tmp.join("v1").join("com.spark.h"), false)
            .unwrap();

        // 同版重装：Updated，不报错。
        make_webview_plugin_ver(
            &tmp.join("v2").join("com.spark.h"),
            "com.spark.h",
            "h",
            "0.1.0",
        );
        let o = pm
            .install_from_dir(&tmp.join("v2").join("com.spark.h"), false)
            .unwrap();
        assert_eq!(o.action, InstallAction::Updated);
        assert_eq!(o.previous_version.as_deref(), Some("0.1.0"));
    }

    #[test]
    fn install_downgrade_needs_confirm() {
        let tmp = std::env::temp_dir().join("spark_pm_down");
        let _ = fs::remove_dir_all(&tmp);
        // 先装 0.2.0
        make_webview_plugin_ver(
            &tmp.join("v2").join("com.spark.h"),
            "com.spark.h",
            "h",
            "0.2.0",
        );
        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        pm.install_from_dir(&tmp.join("v2").join("com.spark.h"), false)
            .unwrap();
        let installed_json = tmp.join("plugins").join("com.spark.h").join("plugin.json");
        let installed_before = fs::read_to_string(&installed_json).unwrap();

        // 再装 0.1.0（旧版）：force=false 应返回 ConfirmDowngrade 且不写盘。
        make_webview_plugin_ver(
            &tmp.join("v1").join("com.spark.h"),
            "com.spark.h",
            "h",
            "0.1.0",
        );
        let o = pm
            .install_from_dir(&tmp.join("v1").join("com.spark.h"), false)
            .unwrap();
        assert_eq!(o.action, InstallAction::ConfirmDowngrade);
        assert_eq!(o.version, "0.1.0");
        assert_eq!(o.previous_version.as_deref(), Some("0.2.0"));
        // 盘上版本未变。
        assert_eq!(
            fs::read_to_string(&installed_json).unwrap(),
            installed_before
        );
        assert_eq!(pm.list()[0].version, "0.2.0");

        // force=true 强制覆盖：Updated，版本变 0.1.0。
        let o = pm
            .install_from_dir(&tmp.join("v1").join("com.spark.h"), true)
            .unwrap();
        assert_eq!(o.action, InstallAction::Updated);
        assert_eq!(o.version, "0.1.0");
        assert_eq!(pm.list()[0].version, "0.1.0");
    }

    #[test]
    fn install_preserves_state() {
        let tmp = std::env::temp_dir().join("spark_pm_state");
        let _ = fs::remove_dir_all(&tmp);
        // 先装一个声明 clipboard+notify 权限的插件，授权两项并禁用。
        let dir1 = tmp.join("v1").join("com.spark.h");
        fs::create_dir_all(&dir1).unwrap();
        let json1 = r#"{
            "id": "com.spark.h", "name": "T", "version": "0.1.0", "api_version": 2,
            "runtime": "webview", "main": "index.html",
            "features": [{ "type": "keyword", "keyword": "h", "title": "T", "mode": "page" }],
            "permissions": ["clipboard", "notify"]
        }"#;
        fs::write(dir1.join("plugin.json"), json1).unwrap();
        fs::write(dir1.join("index.html"), "<html></html>").unwrap();
        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        pm.install_from_dir(&dir1, false).unwrap();
        pm.grant("com.spark.h", vec!["clipboard".into(), "notify".into()])
            .unwrap();
        pm.set_enabled("com.spark.h", false).unwrap();

        // 升级到 0.2.0，但新版本只声明 clipboard（删了 notify）。
        let dir2 = tmp.join("v2").join("com.spark.h");
        fs::create_dir_all(&dir2).unwrap();
        let json2 = r#"{
            "id": "com.spark.h", "name": "T", "version": "0.2.0", "api_version": 2,
            "runtime": "webview", "main": "index.html",
            "features": [{ "type": "keyword", "keyword": "h", "title": "T", "mode": "page" }],
            "permissions": ["clipboard"]
        }"#;
        fs::write(dir2.join("plugin.json"), json2).unwrap();
        fs::write(dir2.join("index.html"), "<html></html>").unwrap();
        pm.install_from_dir(&dir2, false).unwrap();

        // enabled 保持禁用；granted 保留 clipboard，裁剪掉不再声明的 notify。
        let p = pm
            .list()
            .into_iter()
            .find(|p| p.id == "com.spark.h")
            .unwrap();
        assert!(!p.enabled);
        assert_eq!(p.granted, vec!["clipboard".to_string()]);

        // 裁剪后的 granted 已持久化：从同一 data_dir 重载状态后仍为裁剪集（不含 notify）。
        let fresh = PluginState::load_at(&tmp.join("data").join("plugins-state.json"));
        assert_eq!(
            fresh.granted_of("com.spark.h"),
            vec!["clipboard".to_string()]
        );
    }

    #[test]
    fn scan_skips_staging_residue_and_cleans_up() {
        let tmp = std::env::temp_dir().join("spark_pm_residue_scan");
        let _ = fs::remove_dir_all(&tmp);
        let plugins = tmp.join("plugins");
        // 真实插件
        make_webview_plugin_ver(&plugins.join("com.spark.h"), "com.spark.h", "h", "0.1.0");
        // 模拟上次覆盖安装崩溃残留的暂存/备份（含合法 plugin.json）
        make_webview_plugin_ver(
            &plugins.join(".com.spark.h.staging"),
            "com.spark.h",
            "h",
            "0.2.0",
        );
        make_webview_plugin_ver(
            &plugins.join(".com.spark.h.bak"),
            "com.spark.h",
            "h",
            "0.1.0",
        );

        // scan_standard 应清理残留 + 跳过点目录，只加载真实插件，不产生僵尸条目。
        let mut pm = PluginManager::with_dirs(plugins.clone(), tmp.join("data"));
        pm.scan_standard().unwrap();
        assert_eq!(pm.list().len(), 1);
        assert_eq!(pm.list()[0].id, "com.spark.h");
        // 残留已被启动清理删除。
        assert!(!plugins.join(".com.spark.h.staging").exists());
        assert!(!plugins.join(".com.spark.h.bak").exists());
    }
}
