//! Plugin discovery, manifest handling, install/lifecycle, and state.
//!
//! webview 插件：features 关键字路由 + 搜索框 `plugin:page:` 候选开窗。
//! native 插件：**纯应用**模型——必须有 `page`（HTML 入口），exe 只经 `plugin.page`
//! RPC 服务于页面（native.rs 模块注释）；旧 commands/keywords（exe 直接向搜索框
//! 应答的 list 模式）清单校验直接拒绝；features 与 webview 同构（仅 page 模式），
//! 关键字在搜索框产出候选、打开同一套页面窗口。

mod db;
mod error;
mod manifest;
mod native;
mod signing;
mod state;

pub use error::PluginError;
pub use manifest::{
    cmp_version, FeatureType, PluginCommand, PluginFeature, PluginManifest, PluginRuntime,
    PluginWindow,
};
pub use native::{NativePageRequest, NativeRuntimeHandle, NativeSpawnInfo};
pub use signing::{
    canonical_bytes, collect_file_entries, verify_dir, verify_with_keys, FileEntry, KeyKind,
    Revocation, SignState, TrustedKey, VerifyError, REVOKED, TRUSTED_KEYS,
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
    /// 签名状态：install 时全量验签写入；scan 时轻量验签写入。
    /// dev 插件恒为 `Unsigned`（开发者本地目录，不参与签名体系）。
    pub sign_state: SignState,
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
    /// 插件是否拥有可打开的页面（webview：有 mode:page feature；native：声明了
    /// page 字段）。UI 据此决定插件卡片是否显示「打开」按钮。
    pub has_page: bool,
    /// 签名状态：`official` / `third_party` / `unsigned` / `invalid`（snake_case 序列化）。
    pub sign_state: SignState,
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
    /// 本次安装验签结果。UI 据此决定是否展示"官方"角标。
    pub sign_state: SignState,
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

/// 不 derive Debug：native 运行时句柄背后是子进程（Child/管道），不可 Debug。
/// Default 仍可用：所有字段均有 Default（句柄 Default = 启动专职 runtime 线程）。
#[derive(Default)]
pub struct PluginManager {
    plugins: Vec<LoadedPlugin>,
    /// 标准（可装卸）插件目录。默认 `<exe_dir>/plugins`，可由 config/设置覆盖。
    plugins_dir: PathBuf,
    /// 状态文件与插件私有数据所在目录（默认全局 data_dir；测试可注入）。
    data_dir: PathBuf,
    state: PluginState,
    /// native 插件进程运行时句柄：runtime 状态生活在专职线程（native.rs 模块注释），
    /// host 锁内只做通道 send / 快照构造，RPC 等待一律在 host 锁外（见
    /// `NativePageRequest` 与 native_shutdown_plugin 的注释）。
    native: NativeRuntimeHandle,
    /// 运行时可信密钥表 = 内置官方表（`TRUSTED_KEYS`）+ 用户导入的三方密钥。
    /// 验签一律走此合并表：官方判定只看内置 `KeyKind::Official` 条目，
    /// 用户表恒为 `KeyKind::ThirdParty`（"已签名"角标），无法伪冒官方。
    trusted_keys: Vec<TrustedKey>,
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
            native: NativeRuntimeHandle::default(),
            trusted_keys: crate::signing::TRUSTED_KEYS.to_vec(),
        }
    }

    /// 替换用户导入的三方密钥表（`HostConfig.trusted_pubkeys` 变更时调用）。
    /// 内置官方表恒定保留：与传入 key_id 冲突的用户条目被丢弃（官方 key_id 不可被
    /// 用户覆盖，否则官方插件会被降级展示为"已签名"）。入参条目若被错误构造为
    /// `KeyKind::Official` 同样丢弃——用户导入的公钥在 host 侧**恒为 ThirdParty**，
    /// 防配置解析/构造失误把用户密钥抬升为"官方"。
    pub fn set_trusted_user_keys(&mut self, user_keys: Vec<TrustedKey>) {
        let builtin = crate::signing::TRUSTED_KEYS;
        let mut merged: Vec<TrustedKey> = builtin.to_vec();
        for k in user_keys {
            if k.kind == KeyKind::ThirdParty
                && !merged
                    .iter()
                    .any(|m| m.key_id.as_ref() == k.key_id.as_ref())
            {
                merged.push(k);
            }
        }
        let prev_user = self.trusted_keys.len().saturating_sub(builtin.len());
        let new_user = merged.len() - builtin.len();
        if prev_user != new_user {
            info!(user_keys = new_user, "trusted user keys table updated");
        }
        self.trusted_keys = merged;
    }

    /// 当前生效的可信密钥表（内置 + 用户导入）。
    pub fn trusted_keys(&self) -> &[TrustedKey] {
        &self.trusted_keys
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

    /// 扫描/启动期验签（3.2 起**全量重算**）：校验 signature.json schema + 重算目录内
    /// 每个文件的哈希与清单双向比对 + Ed25519 验签（用运行时合并的可信表）。
    /// io 错误降级为 `Unsigned`（best-effort，不阻塞插件加载，与既有隔离原则一致）。
    fn sign_state_scanned(root: &Path, id: &str, version: &str, keys: &[TrustedKey]) -> SignState {
        match verify_with_keys(root, id, version, keys, crate::signing::REVOKED) {
            Ok(s) => s,
            Err(e) => {
                warn!(?e, id, "sign verify io error, treating as unsigned");
                SignState::Unsigned
            }
        }
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
        // dev 插件恒为 Unsigned：开发目录是开发者本地产物，不参与签名体系，
        // 即便内含 signature.json 也不展示官方/失效角标（避免误导）。
        self.plugins.push(LoadedPlugin {
            manifest,
            root: dir.to_path_buf(),
            source: PluginSource::Dev,
            enabled,
            granted,
            sign_state: SignState::Unsigned,
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
                    // 全量重验（3.2）：磁盘文件被改 → Invalid，UI 红色提示 + 禁用开关。
                    let sign_state =
                        Self::sign_state_scanned(&path, &id, &manifest.version, &self.trusted_keys);
                    info!(id = %id, path = %path.display(), "loaded plugin manifest");
                    self.plugins.push(LoadedPlugin {
                        manifest,
                        root: path,
                        source,
                        enabled,
                        granted,
                        sign_state,
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
    /// `require_signature` 控制签名策略（规范 §6.2）：
    /// - 源目录有 `signature.json` 且验签失败（哈希不匹配/签名错/key_id 不可信）
    ///   → 一律 `Err(SignatureInvalid)`，无论 `require_signature` 取值（破损签名拒装）。
    /// - 无 `signature.json`：`require_signature=true` → `Err(SignatureMissing)`；
    ///   `require_signature=false` → 记 `Unsigned`，继续安装。
    /// - 验过 → 记 `Official`/`ThirdParty`。
    /// 验签在拷贝前对**源目录**做全量重算（`verify_dir`），防止把破损签名写盘。
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
        require_signature: bool,
    ) -> Result<PluginInstallOutcome, PluginError> {
        let manifest = PluginManifest::load(&src.join("plugin.json"))?;
        let id = manifest.id.clone();
        let new_version = manifest.version.clone();
        let dest = self.plugins_dir.join(&id);

        // 拷贝前对源目录全量验签（用运行时合并可信表）：破损签名拒装，无签名按策略决定。
        let verify_result = verify_with_keys(
            src,
            &id,
            &new_version,
            &self.trusted_keys,
            crate::signing::REVOKED,
        )?;
        let sign_state = enforce_install_signature(&id, verify_result, require_signature)?;

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
                sign_state,
            });
            info!(id = %id, "installed plugin");
            self.warn_keyword_conflicts();
            return Ok(PluginInstallOutcome {
                id,
                action: InstallAction::Installed,
                version: new_version,
                previous_version: None,
                sign_state,
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
                sign_state,
            });
        }

        // 覆盖更新：暂存拷贝 + 备份交换，保证拷贝失败不破坏现有插件
        // （与 set_dir 迁移的"先拷后删+失败回滚"安全模式一致）。
        // 同步等待进程退出：.exe 被占用时覆盖会失败（见 shutdown_plugin_sync）。
        // 未确认退出（3s 盖帽，如 runtime 线程被在途 PageCall 占住）即中止——
        // 后面的 rename+备份覆盖事务不会发生，旧插件保持完整可用。
        if !self.native.shutdown_plugin_sync(&id) {
            return Err(PluginError::Manifest(format!(
                "plugin {id} process did not exit within 3s, update aborted"
            )));
        }
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
            sign_state,
        });
        info!(id = %id, new = %new_version, old = %old_version, "updated plugin");
        self.warn_keyword_conflicts();
        Ok(PluginInstallOutcome {
            id,
            action: InstallAction::Updated,
            version: new_version,
            previous_version: Some(old_version),
            sign_state,
        })
    }

    /// 卸载标准来源插件（开发插件不可卸载）。
    pub fn uninstall(&mut self, id: &str) -> Result<(), PluginError> {
        let pos = self
            .plugins
            .iter()
            .position(|p| p.manifest.id == id && p.source == PluginSource::Standard)
            .ok_or_else(|| PluginError::Manifest(format!("plugin not found: {id}")))?;
        // native exe 可能仍在运行（关窗的 page_closed 通知是异步的，可能还在路上）：
        // 先同步关停进程，否则 remove_dir_all 删不掉被占用的 .exe。webview 无进程，空操作。
        // 未确认退出即中止删除——remove_dir_all 半删会留下损坏目录（plugin.json 已删、
        // exe 残留），返回错误让 UI 提示稍后重试。
        if !self.native.shutdown_plugin_sync(id) {
            return Err(PluginError::Manifest(format!(
                "plugin {id} process did not exit within 3s, uninstall aborted"
            )));
        }
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
                has_page: Self::has_page(p),
                sign_state: p.sign_state,
            })
            .collect()
    }

    /// 插件是否拥有可打开的页面（list() 与「打开」按钮判定共用）。
    fn has_page(p: &LoadedPlugin) -> bool {
        match p.manifest.runtime {
            PluginRuntime::Native => p.manifest.page.is_some(),
            _ => p.manifest.features.iter().any(|f| f.mode == "page"),
        }
    }

    pub fn open(&self, id: &str) -> Result<PluginOpenInfo, PluginError> {
        let p = self
            .plugins
            .iter()
            .find(|p| p.manifest.id == id && p.enabled)
            .ok_or_else(|| PluginError::Manifest(format!("plugin not found or disabled: {id}")))?;
        // 入口 HTML：webview 用 main，native（纯应用）用 page。
        let entry_rel = match p.manifest.runtime {
            PluginRuntime::Webview => p.manifest.main.clone(),
            PluginRuntime::Native => p
                .manifest
                .page
                .clone()
                .ok_or_else(|| PluginError::Manifest(format!("plugin {id} has no page")))?,
            PluginRuntime::Wasm => {
                return Err(PluginError::Manifest(format!(
                    "plugin {id} runtime is not openable"
                )));
            }
        };
        let main_abs = p.root.join(entry_rel);
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

    /// 查询路由：在 enabled 插件的 page 模式关键字中匹配（webview 与 native 同构，
    /// native 的关键字候选打开同一套页面窗口，exe 不直接应答搜索）。
    pub fn find_keyword_match(&self, text: &str) -> Option<KeywordMatch> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }
        let lower = trimmed.to_ascii_lowercase();
        for p in &self.plugins {
            if !p.enabled {
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
                        // 按字节偏移取原始大小写输入：to_ascii_lowercase 只改 1 字节 ASCII
                        // 字符，不改变字节长度与 UTF-8 字符边界，故对中文关键字同样安全。
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

    /// 搜索前缀建议：输入是某已启用 page 插件（webview/native）关键字的**真前缀**（还没打完）时产出候选，
    /// 供主列表随输入即时展示（uTools 同款：打 "内" 就能看到"内容对比"）。
    /// 与 `find_keyword_match`（可打开判定，要求完整关键字）解耦：
    /// 返回的 match 与精确命中同构（input 为空），打开时即以关键字本身进入插件页。
    pub fn find_keyword_prefix_matches(&self, text: &str) -> Vec<KeywordMatch> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }
        // 带空格 = 已进入"关键字 + 参数"阶段，由 find_keyword_match 精确路由，不再给前缀建议
        if trimmed.contains(' ') {
            return Vec::new();
        }
        let lower = trimmed.to_ascii_lowercase();
        let mut out = Vec::new();
        for p in &self.plugins {
            if !p.enabled {
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
                    // 真前缀：关键字严格长于输入且以输入开头（排除完整命中，那由
                    // find_keyword_match 以更高优先级处理，避免同词双候选）。
                    if kw_l.len() > lower.len() && kw_l.starts_with(&lower)
                        // 路由语义是清单内首个匹配 feature 胜出；同插件重复关键字
                        // 再产候选只会得到两行完全同 id 的结果，此处去重。
                        && !out
                            .iter()
                            .any(|e: &KeywordMatch| e.plugin_id == p.manifest.id && e.keyword == kw)
                    {
                        out.push(KeywordMatch {
                            plugin_id: p.manifest.id.clone(),
                            feature_index: fi,
                            input: String::new(),
                            keyword: kw.to_string(),
                        });
                    }
                }
            }
        }
        out
    }

    /// 检测已启用插件间的 page 关键字冲突（同一关键字被多个插件占用；webview 与
    /// native 同一搜索路由，native 关键字同样计入）。
    ///
    /// 仅统计 `enabled` 的插件：禁用的插件不参与路由，不构成实际冲突。
    /// 路由仍按加载顺序首匹配胜出（`find_keyword_match`）；本方法只暴露冲突供日志/设置页提示。
    pub fn keyword_conflicts(&self) -> Vec<KeywordConflict> {
        use std::collections::BTreeMap;
        let mut by_kw: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for p in &self.plugins {
            if !p.enabled {
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

    /// 构造某 native 插件的启动信息（owned，可跨锁阶段传递）。
    pub fn native_spawn_info(&self, id: &str) -> Option<NativeSpawnInfo> {
        self.native_plugin(id).map(NativeSpawnInfo::from_plugin)
    }

    /// native 页面转发调用的**锁内准备段**（纯应用模型唯一 RPC）：鉴权后的调用方
    /// 在 host 锁内只做快照构造，随后**放锁**再 `NativePageRequest::execute`——
    /// 懒启动 spawn + 5s RPC + 15s 盖帽的等待全部在 host 锁外（native.rs 模块
    /// 注释的锁序纪律）。插件不存在/未启用 → Err（host 侧转 UNAVAILABLE）。
    pub fn native_page_request(
        &self,
        id: &str,
        method: &str,
        args: serde_json::Value,
    ) -> Result<NativePageRequest, PluginError> {
        let info = self
            .native_spawn_info(id)
            .ok_or_else(|| PluginError::Manifest(format!("native plugin not found: {id}")))?;
        Ok(NativePageRequest::new(
            self.native.clone(),
            info,
            method.to_string(),
            args,
        ))
    }

    /// 关停指定 native 插件进程，**不等待**（页面关闭通知路径）：host 锁内只花
    /// 一次通道 send。进程未运行（含 webview id）为 no-op。
    pub fn native_shutdown_plugin(&self, id: &str) {
        self.native.shutdown_plugin(id);
    }

    /// 关停指定 native 插件进程并**同步等待**（覆盖更新/卸载前：确认 .exe 已不被占用）。
    /// 返回 false = 3s 内未确认退出，调用方应中止删改。
    pub fn native_shutdown_plugin_sync(&self, id: &str) -> bool {
        self.native.shutdown_plugin_sync(id)
    }

    /// host 退出前调用：向所有 native 进程发 shutdown 并 wait，收割 runtime 线程。
    pub fn native_shutdown_all(&mut self) {
        self.native.shutdown_all();
    }
}

/// §6.2 安装签名策略（独立成函数便于单测固定；install_from_dir 内部引用）：
/// - `Invalid` 一律拒装（破损签名=可能被篡改，不分 `require_signature`，永不装）。
/// - `Unsigned` + `require_signature=true` → 拒装（`SignatureMissing`）。
/// - 其余（官方/三方验过、或不要求时的无签名）放行。
pub(crate) fn enforce_install_signature(
    id: &str,
    state: SignState,
    require_signature: bool,
) -> Result<SignState, PluginError> {
    match state {
        SignState::Invalid => Err(PluginError::SignatureInvalid(id.to_string())),
        SignState::Unsigned if require_signature => {
            Err(PluginError::SignatureMissing(id.to_string()))
        }
        s => Ok(s),
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
    use base64::Engine;
    use ed25519_dalek::{Signer, SigningKey};
    use std::borrow::Cow;
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

    /// native 纯应用插件 fixture（page 模型：无 commands，必有 page）。
    fn make_native_page_plugin(dir: &Path, id: &str) {
        make_native_page_plugin_kw(dir, id, None);
    }

    /// 同上，可选 features 关键字（page 模式）：native 的搜索框入口。
    fn make_native_page_plugin_kw(dir: &Path, id: &str, keyword: Option<&str>) {
        fs::create_dir_all(dir).unwrap();
        let features = match keyword {
            Some(kw) => format!(
                r#", "features": [{{ "type": "keyword", "keyword": "{kw}", "title": "N", "mode": "page" }}]"#
            ),
            None => String::new(),
        };
        let json = format!(
            r#"{{ "id": "{id}", "name": "N", "version": "0.1.0", "api_version": 2,
                 "runtime": "native", "main": "{id}.exe", "page": "page.html"{features} }}"#
        );
        fs::write(dir.join("plugin.json"), json).unwrap();
        fs::write(dir.join("page.html"), "<html></html>").unwrap();
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
    fn keyword_match_native_same_as_webview() {
        // native 的 features（page 模式）关键字与 webview 同构：精确命中 + 前缀
        // 建议 + 参数切片一致；禁用同样跳过；无 features 的 native 不产生候选。
        let tmp = std::env::temp_dir().join("spark_pm_kw_native");
        let _ = fs::remove_dir_all(&tmp);
        make_native_page_plugin_kw(&tmp.join("com.spark.np"), "com.spark.np", Some("echo"));
        make_native_page_plugin(&tmp.join("com.spark.bare"), "com.spark.bare");
        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        pm.load_dev_dir(&tmp.join("com.spark.np")).unwrap();
        pm.load_dev_dir(&tmp.join("com.spark.bare")).unwrap();

        let m = pm.find_keyword_match("echo").unwrap();
        assert_eq!(m.plugin_id, "com.spark.np");
        assert!(m.input.is_empty());

        let m2 = pm.find_keyword_match("echo hi").unwrap();
        assert_eq!(m2.input, "hi");

        let sug = pm.find_keyword_prefix_matches("ech");
        assert_eq!(sug.len(), 1);
        assert_eq!(sug[0].plugin_id, "com.spark.np");

        // 无 features 的 native 插件：不进搜索路由（页面走卡片「打开」）。
        assert!(pm.find_keyword_match("bare").is_none());
        assert!(pm.find_keyword_prefix_matches("bar").is_empty());

        // 冲突检测把 native 关键字一并计入。
        make_webview_plugin(&tmp.join("com.spark.wv"), "com.spark.wv", "echo");
        pm.load_dev_dir(&tmp.join("com.spark.wv")).unwrap();
        let conflicts = pm.keyword_conflicts();
        assert!(conflicts.iter().any(|c| c.keyword == "echo"
            && c.plugin_ids.contains(&"com.spark.np".to_string())
            && c.plugin_ids.contains(&"com.spark.wv".to_string())));
    }

    #[test]
    fn keyword_match_chinese() {
        // 中文关键字：精确命中 + 前缀带参，且字节偏移切片不 panic、参数完整。
        let tmp = std::env::temp_dir().join("spark_pm_kw_cn");
        let _ = fs::remove_dir_all(&tmp);
        make_webview_plugin(&tmp.join("com.spark.fy"), "com.spark.fy", "翻译");
        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        pm.load_dev_dir(&tmp.join("com.spark.fy")).unwrap();

        let m = pm.find_keyword_match("翻译").unwrap();
        assert_eq!(m.plugin_id, "com.spark.fy");
        assert!(m.input.is_empty());
        assert_eq!(m.keyword, "翻译");

        let m2 = pm.find_keyword_match("翻译 hello world").unwrap();
        assert_eq!(m2.input, "hello world");
        assert_eq!(m2.keyword, "翻译");

        // 大小写混排 ASCII 与中文混打的输入也要保真。
        let m3 = pm.find_keyword_match("翻译 HeLLo").unwrap();
        assert_eq!(m3.input, "HeLLo");

        // 非该关键字前缀不误命中。
        assert!(pm.find_keyword_match("翻 译").is_none());
    }

    #[test]
    fn keyword_prefix_suggest() {
        // 前缀建议：真前缀产出候选（input 为空）；完整命中不重复建议；
        // 带空格（参数阶段）/空输入不产生建议；ASCII 大小写归一同样生效。
        let tmp = std::env::temp_dir().join("spark_pm_kw_pre");
        let _ = fs::remove_dir_all(&tmp);
        make_webview_plugin(&tmp.join("com.spark.fy"), "com.spark.fy", "翻译");
        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        pm.load_dev_dir(&tmp.join("com.spark.fy")).unwrap();

        let hits = pm.find_keyword_prefix_matches("翻");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].plugin_id, "com.spark.fy");
        assert_eq!(hits[0].keyword, "翻译");
        assert!(hits[0].input.is_empty());

        // 完整命中不进前缀列表（由 find_keyword_match 高优先级处理）
        assert!(pm.find_keyword_prefix_matches("翻译").is_empty());
        // 非关键字前缀不误命中
        assert!(pm.find_keyword_prefix_matches("翻译x").is_empty());
        // 带空格进入参数阶段交给精确路由；空输入不给建议
        assert!(pm.find_keyword_prefix_matches("翻 译").is_empty());
        assert!(pm.find_keyword_prefix_matches("").is_empty());

        make_webview_plugin(&tmp.join("com.spark.tr"), "com.spark.tr", "tr");
        pm.load_dev_dir(&tmp.join("com.spark.tr")).unwrap();
        assert!(pm
            .find_keyword_prefix_matches("T")
            .iter()
            .any(|m| m.keyword == "tr"));
    }

    #[test]
    fn keyword_prefix_suggest_dedup_same_plugin_duplicate_keyword() {
        // 同插件清单内两个 feature 声明同一 page 关键字（合法形状，路由首者胜）：
        // 前缀建议按 (plugin_id, keyword) 去重，不得出现两条同 id 候选。
        let tmp = std::env::temp_dir().join("spark_pm_kw_pre_dup");
        let _ = fs::remove_dir_all(&tmp);
        let dir = tmp.join("com.spark.dup");
        std::fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("plugin.json"),
            r#"{ "id": "com.spark.dup", "name": "T", "version": "0.1.0", "api_version": 2,
                 "runtime": "webview", "main": "index.html",
                 "features": [
                   { "type": "keyword", "keyword": "翻译", "title": "T", "mode": "page" },
                   { "type": "keyword", "keyword": "翻译", "title": "T-dup", "mode": "page" }
                 ] }"#,
        )
        .unwrap();
        fs::write(dir.join("index.html"), "<html></html>").unwrap();
        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        pm.load_dev_dir(&dir).unwrap();

        let hits = pm.find_keyword_prefix_matches("翻");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].plugin_id, "com.spark.dup");
        assert_eq!(hits[0].feature_index, 0); // 首个匹配 feature 胜出
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
            .install_from_dir(&tmp.join("src").join("com.spark.hello"), false, false)
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
    fn open_native_page_plugin() {
        // native 纯应用：open 返回 page.html 入口；page 文件缺失时拒绝（exe 缺失不管，
        // exe 只在页面首次 RPC 时才需要）。
        let tmp = std::env::temp_dir().join("spark_pm_open_native");
        let _ = fs::remove_dir_all(&tmp);
        let dir = tmp.join("com.spark.np");
        fs::create_dir_all(&dir).unwrap();
        let json = r#"{
            "id":"com.spark.np","name":"N","version":"0.1.0","api_version":2,
            "runtime":"native","main":"np.exe","page":"page.html"
        }"#;
        fs::write(dir.join("plugin.json"), json).unwrap();
        fs::write(dir.join("page.html"), "<html></html>").unwrap();
        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        pm.load_dev_dir(&dir).unwrap();
        let info = pm.open("com.spark.np").unwrap();
        assert!(info.main_abs.ends_with("page.html"));
        // list DTO 的 has_page 对 native 纯应用恒为 true（「打开」按钮依据）。
        assert!(pm
            .list()
            .iter()
            .any(|p| p.id == "com.spark.np" && p.has_page));

        // page 文件缺失 → 拒绝开窗。
        fs::remove_file(dir.join("page.html")).unwrap();
        let mut pm2 = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        pm2.load_dev_dir(&dir).unwrap();
        assert!(pm2.open("com.spark.np").is_err());
        // 禁用 → 拒绝。
        pm2.set_enabled("com.spark.np", false).unwrap();
        assert!(pm2.open("com.spark.np").is_err());
    }

    #[test]
    fn native_page_request_rejects_non_native() {
        // rpc 能力仅 native 插件可用：webview 插件无 exe 可转发，准备段即报错
        // （快照构造不涉及 spawn/RPC 等待，可直接在测试中调用）。
        let tmp = std::env::temp_dir().join("spark_pm_pagereq_nonnative");
        let _ = fs::remove_dir_all(&tmp);
        make_webview_plugin(&tmp.join("com.spark.w"), "com.spark.w", "w");
        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        pm.load_dev_dir(&tmp.join("com.spark.w")).unwrap();
        let err = pm
            .native_page_request("com.spark.w", "m", serde_json::Value::Null)
            .unwrap_err();
        assert!(matches!(err, PluginError::Manifest(_)));
        assert!(pm
            .native_page_request("com.spark.absent", "m", serde_json::Value::Null)
            .is_err());
    }

    #[test]
    fn native_page_request_disabled_plugin_rejected() {
        // 禁用的 native 插件不再接受页面转发（开新页面已被 open() 拦，旧页面
        // 的在途调用由关窗 shutdown 收尾）。
        let tmp = std::env::temp_dir().join("spark_pm_pagereq_disabled");
        let _ = fs::remove_dir_all(&tmp);
        make_native_page_plugin(&tmp.join("com.spark.nd"), "com.spark.nd");
        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        pm.load_dev_dir(&tmp.join("com.spark.nd")).unwrap();
        pm.set_enabled("com.spark.nd", false).unwrap();
        assert!(pm
            .native_page_request("com.spark.nd", "m", serde_json::Value::Null)
            .is_err());
        // 关停请求对禁用/未运行插件是幂等 no-op（关窗通知不依赖 enabled）。
        pm.native_shutdown_plugin("com.spark.nd");
        pm.native_shutdown_plugin_sync("com.spark.nd");
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
            .install_from_dir(&tmp.join("v1").join("com.spark.h"), false, false)
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
            .install_from_dir(&tmp.join("v2").join("com.spark.h"), false, false)
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
        pm.install_from_dir(&tmp.join("v1").join("com.spark.h"), false, false)
            .unwrap();
        make_webview_plugin_ver(
            &tmp.join("v2").join("com.spark.h"),
            "com.spark.h",
            "h",
            "0.2.0",
        );
        pm.install_from_dir(&tmp.join("v2").join("com.spark.h"), false, false)
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
        pm.install_from_dir(&tmp.join("v1").join("com.spark.h"), false, false)
            .unwrap();

        // 同版重装：Updated，不报错。
        make_webview_plugin_ver(
            &tmp.join("v2").join("com.spark.h"),
            "com.spark.h",
            "h",
            "0.1.0",
        );
        let o = pm
            .install_from_dir(&tmp.join("v2").join("com.spark.h"), false, false)
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
        pm.install_from_dir(&tmp.join("v2").join("com.spark.h"), false, false)
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
            .install_from_dir(&tmp.join("v1").join("com.spark.h"), false, false)
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
            .install_from_dir(&tmp.join("v1").join("com.spark.h"), true, false)
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
        pm.install_from_dir(&dir1, false, false).unwrap();
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
        pm.install_from_dir(&dir2, false, false).unwrap();

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

    // ─── 签名策略（Phase 3）─────────────────────────────────────────────────

    #[test]
    fn install_unsigned_without_require_allows() {
        let tmp = std::env::temp_dir().join("spark_pm_sig_unsigned_ok");
        let _ = fs::remove_dir_all(&tmp);
        make_webview_plugin(&tmp.join("src").join("com.spark.h"), "com.spark.h", "h");
        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        // require_signature=false：无 signature.json 仍可装，sign_state=Unsigned。
        let o = pm
            .install_from_dir(&tmp.join("src").join("com.spark.h"), false, false)
            .unwrap();
        assert_eq!(o.sign_state, SignState::Unsigned);
        assert_eq!(pm.list()[0].sign_state, SignState::Unsigned);
    }

    #[test]
    fn install_unsigned_with_require_rejects() {
        let tmp = std::env::temp_dir().join("spark_pm_sig_unsigned_req");
        let _ = fs::remove_dir_all(&tmp);
        make_webview_plugin(&tmp.join("src").join("com.spark.h"), "com.spark.h", "h");
        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        // require_signature=true 且无 signature.json → SignatureMissing，不写盘。
        let err = pm
            .install_from_dir(&tmp.join("src").join("com.spark.h"), false, true)
            .unwrap_err();
        assert!(matches!(err, PluginError::SignatureMissing(_)));
        assert!(pm.list().is_empty());
        assert!(!tmp.join("plugins").join("com.spark.h").exists());
    }

    #[test]
    fn install_broken_signature_rejects() {
        let tmp = std::env::temp_dir().join("spark_pm_sig_broken");
        let _ = fs::remove_dir_all(&tmp);
        let src = tmp.join("src").join("com.spark.h");
        make_webview_plugin(&src, "com.spark.h", "h");
        // 写一份 schema 合法但签名不可信的 signature.json：key_id 不在内置表 → Invalid。
        let sig_json = serde_json::json!({
            "schema": 1,
            "plugin_id": "com.spark.h",
            "version": "0.1.0",
            "algorithm": "ed25519",
            "key_id": "nonexistent-key",
            "files": [{"path": "plugin.json", "sha256": "0000000000000000000000000000000000000000000000000000000000000000"}],
            "signature": "AAAA"
        });
        fs::write(src.join("signature.json"), sig_json.to_string()).unwrap();

        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        // 破损签名一律拒装，无论 require_signature 取值。
        let err = pm.install_from_dir(&src, false, false).unwrap_err();
        assert!(matches!(err, PluginError::SignatureInvalid(_)));
        assert!(pm.list().is_empty());
    }

    #[test]
    fn scan_reports_invalid_signature_state() {
        let tmp = std::env::temp_dir().join("spark_pm_sig_scan_invalid");
        let _ = fs::remove_dir_all(&tmp);
        let plugins = tmp.join("plugins");
        let dir = plugins.join("com.spark.h");
        make_webview_plugin(&dir, "com.spark.h", "h");
        // 放一份不可信的 signature.json：scan 轻量验签记 Invalid，但不拦截加载。
        let sig_json = serde_json::json!({
            "schema": 1,
            "plugin_id": "com.spark.h",
            "version": "0.1.0",
            "algorithm": "ed25519",
            "key_id": "nonexistent-key",
            "files": [{"path": "plugin.json", "sha256": "0000000000000000000000000000000000000000000000000000000000000000"}],
            "signature": "AAAA"
        });
        fs::write(dir.join("signature.json"), sig_json.to_string()).unwrap();

        let mut pm = PluginManager::with_dirs(plugins, tmp.join("data"));
        pm.scan_standard().unwrap();
        assert_eq!(pm.list().len(), 1);
        assert_eq!(pm.list()[0].sign_state, SignState::Invalid);
    }

    #[test]
    fn scan_reports_unsigned_when_no_signature() {
        let tmp = std::env::temp_dir().join("spark_pm_sig_scan_unsigned");
        let _ = fs::remove_dir_all(&tmp);
        let plugins = tmp.join("plugins");
        make_webview_plugin(&plugins.join("com.spark.h"), "com.spark.h", "h");
        let mut pm = PluginManager::with_dirs(plugins, tmp.join("data"));
        pm.scan_standard().unwrap();
        assert_eq!(pm.list()[0].sign_state, SignState::Unsigned);
    }

    // ─── Phase 5：三方密钥 / 合并可信表 / 签名策略（规范 §5.3、§10、§6.2）────

    /// 测试辅助：给插件目录写一份用给定私钥签的 signature.json。
    fn sign_plugin_dir(dir: &Path, id: &str, version: &str, key_id: &str, sk: &SigningKey) {
        let entries: Vec<FileEntry> = collect_file_entries(dir).unwrap();
        let canon = canonical_bytes(id, version, "ed25519", key_id, &entries);
        let sig = sk.sign(&canon);
        let sig_b64 = base64::engine::general_purpose::STANDARD.encode(sig.to_bytes());
        let files_json: Vec<serde_json::Value> = entries
            .iter()
            .map(|e| serde_json::json!({"path": e.path, "sha256": e.sha256}))
            .collect();
        let sig_json = serde_json::json!({
            "schema": 1,
            "plugin_id": id,
            "version": version,
            "algorithm": "ed25519",
            "key_id": key_id,
            "signed_at": "2026-08-25T10:00:00Z",
            "files": files_json,
            "signature": sig_b64,
        });
        fs::write(
            dir.join("signature.json"),
            serde_json::to_string_pretty(&sig_json).unwrap(),
        )
        .unwrap();
    }

    /// 测试辅助：构造一条用户导入的三方密钥（KeyKind::ThirdParty）。
    fn user_key_for(key_id: &str, sk: &SigningKey) -> TrustedKey {
        let pubkey =
            base64::engine::general_purpose::STANDARD.encode(sk.verifying_key().to_bytes());
        TrustedKey {
            key_id: Cow::Owned(key_id.to_string()),
            algorithm: Cow::Borrowed("ed25519"),
            public_key: Cow::Owned(pubkey),
            kind: KeyKind::ThirdParty,
            note: Cow::Borrowed("test user key"),
        }
    }

    #[test]
    fn install_signature_policy_is_stable() {
        // §6.2 策略矩阵的映射函数：破损签名一律拒；Unsigned+require 拒；其余放行。
        assert!(matches!(
            enforce_install_signature("p", SignState::Invalid, false),
            Err(PluginError::SignatureInvalid(_))
        ));
        assert!(matches!(
            enforce_install_signature("p", SignState::Invalid, true),
            Err(PluginError::SignatureInvalid(_))
        ));
        assert!(matches!(
            enforce_install_signature("p", SignState::Unsigned, true),
            Err(PluginError::SignatureMissing(_))
        ));
        assert_eq!(
            enforce_install_signature("p", SignState::Unsigned, false).unwrap(),
            SignState::Unsigned
        );
        assert_eq!(
            enforce_install_signature("p", SignState::Official, false).unwrap(),
            SignState::Official
        );
        assert_eq!(
            enforce_install_signature("p", SignState::ThirdParty, true).unwrap(),
            SignState::ThirdParty
        );
    }

    #[test]
    fn scan_shows_third_party_only_after_trusting_user_key() {
        // 三方密钥签的插件：未信任时 scan 全量验签记 Invalid（key_id 不在可信表）；
        // 导入该公钥后重建 manager → ThirdParty（"已签名"角标）。
        let tmp = std::env::temp_dir().join("spark_pm_sig_thirdparty_scan");
        let _ = fs::remove_dir_all(&tmp);
        let dir = tmp.join("plugins").join("com.dev.tool");
        make_webview_plugin(&dir, "com.dev.tool", "t");
        let sk = SigningKey::from_bytes(&[11; 32]);
        sign_plugin_dir(&dir, "com.dev.tool", "0.1.0", "dev-v1", &sk);

        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        pm.scan_standard().unwrap();
        assert_eq!(pm.list()[0].sign_state, SignState::Invalid);

        let mut pm2 = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data2"));
        pm2.set_trusted_user_keys(vec![user_key_for("dev-v1", &sk)]);
        pm2.scan_standard().unwrap();
        assert_eq!(pm2.list()[0].sign_state, SignState::ThirdParty);
    }

    #[test]
    fn install_user_key_signed_plugin_requires_trust() {
        // 用户密钥签的插件：未信任 → 拒装（SignatureInvalid，破损签名语义）；
        // 导入公钥后 → 装成功且 sign_state=ThirdParty。
        let tmp = std::env::temp_dir().join("spark_pm_sig_thirdparty_install");
        let _ = fs::remove_dir_all(&tmp);
        let src = tmp.join("src").join("com.dev.tool");
        make_webview_plugin(&src, "com.dev.tool", "t");
        let sk = SigningKey::from_bytes(&[13; 32]);
        sign_plugin_dir(&src, "com.dev.tool", "0.1.0", "dev-v1", &sk);

        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        let err = pm.install_from_dir(&src, false, false).unwrap_err();
        assert!(matches!(err, PluginError::SignatureInvalid(_)));
        assert!(pm.list().is_empty());
        assert!(!tmp.join("plugins").join("com.dev.tool").exists());

        let mut pm2 = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        pm2.set_trusted_user_keys(vec![user_key_for("dev-v1", &sk)]);
        let o = pm2.install_from_dir(&src, false, false).unwrap();
        assert_eq!(o.sign_state, SignState::ThirdParty);
        assert_eq!(pm2.list()[0].sign_state, SignState::ThirdParty);
    }

    #[test]
    fn merged_key_table_drops_conflicting_user_entries() {
        // 用户表与内置官方 key_id 冲突 → 丢弃（官方 key_id 不可被用户覆盖）。
        // 重复 key_id 去重；非 ThirdParty 条目（构造失误）同样丢弃。
        let sk = SigningKey::from_bytes(&[17; 32]);
        let mut pm = PluginManager::with_dirs(
            std::env::temp_dir().join("spark_pm_sig_keymerge_plugins"),
            std::env::temp_dir().join("spark_pm_sig_keymerge_data"),
        );
        let mut forged_official = user_key_for("spark-official-v1", &sk);
        forged_official.kind = KeyKind::Official; // 模拟构造失误/恶意配置
        pm.set_trusted_user_keys(vec![
            forged_official,
            user_key_for("dev-v1", &sk),
            user_key_for("dev-v1", &sk),
            user_key_for("dev-v2", &sk),
        ]);
        // 内置官方 + dev-v1 + dev-v2；用户表里不允许出现官方 key_id 的副本。
        assert_eq!(pm.trusted_keys().len(), 3);
        assert!(!pm.trusted_keys().iter().any(|k| {
            k.key_id.as_ref() == "spark-official-v1" && k.kind == KeyKind::ThirdParty
        }));
        assert!(pm
            .trusted_keys()
            .iter()
            .filter(|k| k.kind == KeyKind::ThirdParty)
            .all(|k| k.key_id.as_ref() == "dev-v1" || k.key_id.as_ref() == "dev-v2"));
    }
}
