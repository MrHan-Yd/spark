use crate::builtins;
use crate::clipboard;
use crate::config::HostConfig;
use crate::shell;
use crate::tray::TrayIcon;
use anyhow::{bail, Result};
use base64::Engine;
use spark_core::{ensure_data_dir, Action, Candidate, Query, Source};
use spark_index::{AppIndex, SearchIndex};
use spark_ipc::{InvokeParams, InvokeResult, PluginApiParams, TrustedPubkeyEntry};
use spark_plugin_manager::{
    KeyKind, KeywordMatch, NativePageRequest, PluginInfo, PluginInstallOutcome, PluginManager,
    PluginOpenInfo, TrustedKey,
};
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

pub struct HostApp {
    /// 应用索引：启动期由 index_watch::build_boot_index 后台构建后带 reconcile 换入；
    /// 运行期 30s 定时器检测到 Start Menu 变化后 swap_memory 原子换入。
    pub index: AppIndex,
    plugins: PluginManager,
    pub config: HostConfig,
    /// 托盘图标（win_loop 创建后注入；用于右下角气泡提示）
    pub tray: Option<TrayIcon>,
}

impl HostApp {
    pub fn bootstrap(extra_plugins: Option<&Path>) -> Result<Self> {
        Self::bootstrap_impl(extra_plugins, false)
    }

    /// 快速启动版：Start Menu 全量扫描放后台（配合 index_watch::build_boot_index，
    /// 完成后带 reconcile 换入），热键注册不再被索引构建阻塞。历史仍同步加载，
    /// 默认页（最近使用）立即可用。`--query` 诊断模式保持同步全量版 bootstrap。
    pub fn bootstrap_fast(extra_plugins: Option<&Path>) -> Result<Self> {
        Self::bootstrap_impl(extra_plugins, true)
    }

    fn bootstrap_impl(extra_plugins: Option<&Path>, background_index: bool) -> Result<Self> {
        let _ = ensure_data_dir()?;
        let config = HostConfig::load_or_default();
        let index = if background_index {
            info!("app index deferred to background (hotkey-first boot)");
            AppIndex::with_history_only()
        } else {
            info!("building app index from Start Menu…");
            let idx = AppIndex::with_seed_fallback();
            info!(apps = idx.len(), "app index ready");
            idx
        };

        // 插件目录优先级：CLI `--plugins-dir` > config.plugins_dir > <exe_dir>/plugins。
        // 一期：内置与用户插件统一放此目录（安装器随包发的内置插件 + 用户导入的都在这）。
        let plugins_dir = resolve_plugins_dir(extra_plugins, config.plugins_dir.as_deref());
        let mut plugins = PluginManager::new(plugins_dir);
        // 配置里用户导入的三方公钥并入运行时可信表（规范 §10）。单条非法仅跳过 +
        // warn（配置手工损坏不应让 host 起不来）；SetConfig 路径则严格整体校验。
        let mut user_keys = Vec::new();
        for entry in &config.trusted_pubkeys {
            match parse_trusted_pubkey(entry) {
                Ok(k) => user_keys.push(k),
                Err(msg) => warn!(key_id = %entry.key_id, "skip invalid trusted pubkey: {msg}"),
            }
        }
        plugins.set_trusted_user_keys(user_keys);
        let n = plugins.scan_standard()?;
        if n > 0 {
            info!(count = n, "scanned plugins");
        }

        Ok(Self {
            index,
            plugins,
            config,
            tray: None,
        })
    }
}

/// IPC 搜索三段式的锁内准备结果。
pub struct SearchPrep {
    pub hits: Vec<Candidate>,
    /// 生效 limit（params.limit==0 时取 config.max_results；锁内定好免二次取锁）。
    pub limit: u32,
}

/// `plugin_api` 的执行计划：`Done` = 已在 host 锁内完成、data 即回包；
/// `NativePage` = native 页面转发请求，调用方**放锁后** `execute`（native RPC
/// 等待最坏 15s，绝不占 host 锁）。
pub enum PluginApiOutcome {
    Done(serde_json::Value),
    NativePage(NativePageRequest),
}

/// 手动实现 Debug：deferred 请求持有子进程句柄（不可 derive），委托其手动 Debug。
impl std::fmt::Debug for PluginApiOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Done(v) => f.debug_tuple("Done").field(v).finish(),
            Self::NativePage(req) => f.debug_tuple("NativePage").field(req).finish(),
        }
    }
}

impl HostApp {
    /// 搜索核心（调用方持有 host 锁）：索引 + 内置命令 + 插件关键字/前缀候选
    /// （webview 与 native 的 page 模式同构；native 候选同样只开页面窗口）。
    fn search_core(&mut self, text: &str) -> Vec<Candidate> {
        let q = Query {
            text: text.into(),
            limit: self.config.max_results,
        };
        let mut hits = self.index.search(&q);
        // 插件关键字路由：命中插件（webview/native）的 keyword feature 时，在结果
        // 最前插入一个 page-mode 候选项，UI 见 target="plugin:page:<id>" 即开插件窗口。
        let mut next_top = 0usize; // 前缀建议紧跟精确命中之后（都排在应用结果前面）
        if let Some(m) = self.plugins.find_keyword_match(text) {
            if let Some(cand) = self.build_plugin_candidate(&m) {
                hits.insert(0, cand);
                next_top = 1;
            }
        }
        // 关键字真前缀建议：中文关键字较长，用户敲前缀（如 "内容"）时也应看到插件
        // 候选随列表出现（uTools 同款交互），打完回车即进插件页。
        for m in self.plugins.find_keyword_prefix_matches(text) {
            if let Some(mut cand) = self.build_plugin_candidate(&m) {
                cand.score = 90.0 - next_top as f32; // 精确命中(100)之下的稳定次序
                let at = next_top.min(hits.len());
                hits.insert(at, cand);
                next_top += 1;
            }
        }
        hits
    }

    /// 同步搜索（`--query` 诊断模式 / invoke 兜底路径）。
    pub fn search(&mut self, text: &str) -> Vec<Candidate> {
        self.search_core(text)
    }

    /// IPC 搜索三段式的锁内段：核心搜索 + 生效 limit。
    pub fn search_prep(&mut self, text: &str, client_limit: u32) -> SearchPrep {
        let hits = self.search_core(text);
        let limit = if client_limit == 0 {
            self.config.max_results
        } else {
            client_limit.min(self.config.max_results)
        };
        SearchPrep { hits, limit }
    }

    /// 由关键字匹配构造一个 page-mode 候选项。
    fn build_plugin_candidate(&self, m: &KeywordMatch) -> Option<Candidate> {
        let p = self
            .plugins
            .plugins()
            .iter()
            .find(|p| p.manifest.id == m.plugin_id)?;
        let f = p.manifest.features.get(m.feature_index)?;
        let subtitle = if m.input.is_empty() {
            f.subtitle.clone().or_else(|| Some(p.manifest.name.clone()))
        } else {
            Some(m.input.clone())
        };
        let icon = p
            .manifest
            .icon
            .as_deref()
            .map(|ic| p.root.join(ic).to_string_lossy().into_owned());
        let target = format!("plugin:page:{}", p.manifest.id);
        Some(Candidate {
            id: format!("plugin:{}:{}", p.manifest.id, m.keyword),
            title: f.title.clone(),
            subtitle,
            target: Some(target.clone()),
            icon,
            score: 100.0,
            source: Source::Plugin,
            actions: vec![Action {
                id: "open".into(),
                title: "打开".into(),
                is_default: true,
                target: Some(target),
            }],
            plugin_id: Some(p.manifest.id.clone()),
        })
    }

    pub fn invoke(&mut self, params: &InvokeParams) -> Result<InvokeResult> {
        // 内置系统命令走专用执行层（不依赖 target 文件，也不记历史/上默认页）
        if let Some(spec) = spark_index::builtin::find(&params.item_id) {
            let action = if params.action_id.is_empty() {
                "open"
            } else {
                params.action_id.as_str()
            };
            // 不可逆操作（关机/重启等）：首次回车只返回确认请求，UI 弹窗
            // 确认后以 "confirm" action 重新 invoke 才真正执行。
            if action == "open" {
                if let Some(message) = spec.confirm {
                    return Ok(InvokeResult::Confirm {
                        message: message.into(),
                    });
                }
            }
            return match builtins::execute(spec.id) {
                Ok(builtins::BuiltinOutcome::Close(msg)) => {
                    Ok(InvokeResult::Close { message: Some(msg) })
                }
                Ok(builtins::BuiltinOutcome::CopyText(text)) => {
                    // 右下角气泡提示已复制（utools 同款反馈）；托盘由 host 常驻持有
                    if let Some(tray) = self.tray.as_mut() {
                        tray.show_balloon("已复制到剪贴板", &format!("{}：{text}", spec.title));
                    }
                    Ok(InvokeResult::CopyText { text })
                }
                Err(e) => {
                    warn!(?e, id = %spec.id, "builtin invoke failed");
                    Ok(InvokeResult::ShowError {
                        message: e.to_string(),
                    })
                }
            };
        }

        let item = self
            .index
            .find_by_id(&params.item_id)
            .or_else(|| {
                // Fallback: search title match once
                self.search(&params.text)
                    .into_iter()
                    .find(|c| c.id == params.item_id)
            })
            .ok_or_else(|| anyhow::anyhow!("item not found: {}", params.item_id))?;

        let action = if params.action_id.is_empty() {
            "open"
        } else {
            params.action_id.as_str()
        };

        // Secondary shortcut actions carry their own target (the merged .lnk);
        // built-in actions (open/runas/reveal) always use the row's target.
        let action_target = if matches!(action, "open" | "runas" | "reveal") {
            None
        } else {
            item.actions
                .iter()
                .find(|a| a.id == action)
                .and_then(|a| a.target.clone())
        };
        let target = action_target.as_deref().or(item.target.as_deref());

        match shell::invoke_action(target, action) {
            Ok(()) => {
                self.index.record_usage(&item);
                Ok(InvokeResult::Close {
                    message: Some(format!("已打开 {}", item.title)),
                })
            }
            Err(e) => {
                warn!(?e, id = %item.id, "invoke failed");
                Ok(InvokeResult::ShowError {
                    message: e.to_string(),
                })
            }
        }
    }

    /// 优雅关闭：向所有 native 插件进程发 shutdown。在主消息循环退出后调用。
    pub fn shutdown(&mut self) {
        self.plugins.native_shutdown_all();
    }

    pub fn plugin_count(&self) -> usize {
        self.plugins.plugins().len()
    }

    #[allow(dead_code)]
    pub fn rebuild_index(&mut self) {
        self.index.rebuild_apps();
        info!(apps = self.index.len(), "index rebuilt");
    }

    pub fn set_hotkey_enabled(&mut self, enabled: bool) {
        if self.config.hotkey_enabled == enabled {
            return;
        }
        let old = self.config.hotkey_enabled;
        self.config.hotkey_enabled = enabled;
        if let Err(e) = self.config.save() {
            self.config.hotkey_enabled = old;
            warn!(?e, "config save failed after hotkey toggle");
        }
    }

    // ─── 插件管理（host.plugin.*）──────────────────────────────────────────

    pub fn plugin_list(&self) -> Vec<PluginInfo> {
        self.plugins.list()
    }

    pub fn plugin_install(
        &mut self,
        path: &str,
        force: bool,
        require_signature: bool,
    ) -> Result<PluginInstallOutcome> {
        // 严格模式（规范 §12.2 3.2，默认关）：仅安装带有效签名的插件——
        // 无论调用方（UI）是否传 require_signature，host 侧强制要求。
        let effective_require = require_signature || self.config.strict_mode;
        Ok(self
            .plugins
            .install_from_dir(Path::new(path), force, effective_require)?)
    }

    /// 应用"受信任开发者"三方公钥表（`host.set_config` 变更后调用）。
    /// 任一条目非法即返回 Err（整体拒绝更新），由 IPC 层转 -32602 让 UI 提示具体 key_id。
    pub fn apply_trusted_pubkeys(&mut self, entries: &[TrustedPubkeyEntry]) -> Result<(), String> {
        let mut keys = Vec::with_capacity(entries.len());
        for e in entries {
            keys.push(parse_trusted_pubkey(e)?);
        }
        self.plugins.set_trusted_user_keys(keys);
        Ok(())
    }

    pub fn plugin_uninstall(&mut self, id: &str) -> Result<()> {
        Ok(self.plugins.uninstall(id)?)
    }

    pub fn plugin_toggle(&mut self, id: &str, enabled: bool) -> Result<()> {
        Ok(self.plugins.set_enabled(id, enabled)?)
    }

    pub fn plugin_grant(&mut self, id: &str, perms: Vec<String>) -> Result<()> {
        Ok(self.plugins.grant(id, perms)?)
    }

    pub fn plugin_devload(&mut self, dir: &str) -> Result<String> {
        Ok(self.plugins.load_dev_dir(Path::new(dir))?)
    }

    pub fn plugin_open(&self, id: &str) -> Result<PluginOpenInfo> {
        Ok(self.plugins.open(id)?)
    }

    /// 插件页面窗口关闭通知（UI → host）：native 纯应用插件的 exe 生命周期与页面
    /// 绑定——关窗即优雅关停进程，"不打开就是不用"。webview 插件无进程，无操作。
    ///
    /// 不看 `enabled`：禁用流程先 toggle 再关窗，关停请求到达时插件可能已
    /// disabled——关停对未运行进程是幂等 no-op，闸门只会造成进程泄漏。
    /// fire-and-forget（不等待、不占 host 锁）：覆盖更新/卸载路径另有同步关停兜底。
    pub fn plugin_page_closed(&self, id: &str) {
        self.plugins.native_shutdown_plugin(id);
    }

    /// 更换插件目录并迁移；同步写回 config.plugins_dir 并保存。
    pub fn plugin_set_dir(&mut self, path: &str, migrate: bool) -> Result<()> {
        let new_dir = PathBuf::from(path);
        self.plugins.set_dir(&new_dir, migrate)?;
        self.config.plugins_dir = Some(new_dir);
        self.config.save()?;
        Ok(())
    }

    /// 公共 API：当前无 IPC 调用方，保留供未来 host.plugins_dir 查询使用。
    #[allow(dead_code)]
    pub fn plugins_dir(&self) -> &Path {
        self.plugins.plugins_dir()
    }

    /// `spark.*` 特权能力桥的**锁内准备段**：校验声明+授权后执行 clipboard/notify/db；
    /// native `rpc` 只做快照构造返回 deferred 请求，由调用方**放锁后**
    /// `NativePageRequest::execute`——native RPC 等待（懒启动最坏 15s）绝不占
    /// host 锁（native.rs 模块注释的锁序纪律）。
    pub fn plugin_api(&mut self, params: &PluginApiParams) -> Result<PluginApiOutcome> {
        let declared = self.plugins.declared_permissions(&params.plugin_id);
        let granted = self.plugins.granted(&params.plugin_id);
        // 权限必须清单声明且用户授权（db 默认开放）。
        let has =
            |perm: &str| declared.iter().any(|p| p == perm) && granted.iter().any(|p| p == perm);

        match params.capability.as_str() {
            "db" => Ok(PluginApiOutcome::Done(self.plugins.plugin_db(
                &params.plugin_id,
                &params.method,
                params.args.clone(),
            )?)),
            "clipboard" => {
                if !has("clipboard") {
                    bail!("PERMISSION_DENIED: clipboard");
                }
                Ok(PluginApiOutcome::Done(
                    self.plugin_api_clipboard(&params.method, &params.args)?,
                ))
            }
            "notify" => {
                if !has("notify") {
                    bail!("PERMISSION_DENIED: notify");
                }
                Ok(PluginApiOutcome::Done(
                    self.plugin_api_notify(&params.args)?,
                ))
            }
            "rpc" => {
                // native 纯应用模型专属：页面 spark.rpc → host → 插件 exe 的
                // plugin.page。不设新权限——native exe 本就拥有完整 OS 能力，
                // 页面与 exe 同源同信任级（与 db 默认开放同理由）；
                // webview 插件没有 exe，明确 UNAVAILABLE 而非静默失败。
                match self.plugins.native_page_request(
                    &params.plugin_id,
                    &params.method,
                    params.args.clone(),
                ) {
                    Ok(req) => Ok(PluginApiOutcome::NativePage(req)),
                    // 错误码语义与 preload 的 ClassifyError 对齐：不可用类失败
                    // 统一 UNAVAILABLE 前缀。
                    Err(e) => bail!("UNAVAILABLE: {e}"),
                }
            }
            other => bail!("UNAVAILABLE: capability {other}"),
        }
    }

    fn plugin_api_clipboard(
        &self,
        method: &str,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        match method {
            "read_text" => {
                let text = clipboard::read_text()?;
                Ok(serde_json::json!({ "text": text }))
            }
            "write_text" => {
                #[derive(serde::Deserialize)]
                struct Args {
                    text: String,
                }
                let a: Args = serde_json::from_value(args.clone())?;
                clipboard::write_text(&a.text)?;
                Ok(serde_json::json!({ "ok": true }))
            }
            // preload 已声明 readImage，但 host 端图片编码尚未实现；
            // 返回明确 UNAVAILABLE 而非 INVALID_ARGS，避免开发者误判为参数错误。
            "read_image" => bail!("UNAVAILABLE: clipboard.read_image 尚未实现"),
            other => bail!("INVALID_ARGS: clipboard method {other}"),
        }
    }

    fn plugin_api_notify(&mut self, args: &serde_json::Value) -> Result<serde_json::Value> {
        #[derive(serde::Deserialize)]
        struct Args {
            title: String,
            #[serde(default)]
            body: Option<String>,
        }
        let a: Args = serde_json::from_value(args.clone())?;
        if let Some(tray) = self.tray.as_mut() {
            tray.show_balloon(&a.title, a.body.as_deref().unwrap_or(""));
        }
        Ok(serde_json::json!({ "ok": true }))
    }
}

/// 解析配置中的一条用户三方公钥（规范 §10）。校验不通过返回带 key_id 的错误信息。
///
/// `KeyKind` 恒为 `ThirdParty`（硬编码，不随配置/网络解析）——"官方"判定只能来自
/// host 内置 `TRUSTED_KEYS`，配置再怎么改都无法把用户密钥抬升为官方。
fn parse_trusted_pubkey(e: &TrustedPubkeyEntry) -> Result<TrustedKey, String> {
    let key_id = e.key_id.trim();
    if key_id.is_empty() {
        return Err("key_id 不能为空".into());
    }
    if key_id.len() > 128 {
        return Err(format!("key_id 过长（>128）：{key_id}"));
    }
    if e.public_key.trim().is_empty() {
        return Err(format!("{key_id}: public_key 不能为空"));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(e.public_key.trim())
        .map_err(|_| format!("{key_id}: public_key 不是合法 base64"))?;
    if bytes.len() != 32 {
        return Err(format!(
            "{key_id}: public_key 必须解码为 32 字节（Ed25519 公钥），实际 {}",
            bytes.len()
        ));
    }
    Ok(TrustedKey {
        key_id: Cow::Owned(key_id.to_string()),
        algorithm: Cow::Borrowed("ed25519"),
        public_key: Cow::Owned(e.public_key.trim().to_string()),
        kind: KeyKind::ThirdParty,
        note: Cow::Owned(e.note.trim().to_string()),
    })
}

/// 解析插件目录：CLI > config > <exe_dir>/plugins；相对路径按 host 目录解析。
fn resolve_plugins_dir(cli: Option<&Path>, config: Option<&Path>) -> PathBuf {
    let chosen = cli.or(config);
    match chosen {
        Some(dir) => {
            if dir.is_relative() {
                crate::exe_dir()
                    .map(|base| base.join(dir))
                    .unwrap_or_else(|| dir.to_path_buf())
            } else {
                dir.to_path_buf()
            }
        }
        None => crate::exe_dir()
            .map(|d| d.join("plugins"))
            .unwrap_or_else(|| PathBuf::from("plugins")),
    }
}

/// Shared host state for message loop callbacks.
pub type SharedHost = Arc<Mutex<HostApp>>;

pub fn share(app: HostApp) -> SharedHost {
    Arc::new(Mutex::new(app))
}

/// Dev helper: print and optionally launch first hit.
pub fn dev_invoke_first(app: &mut HostApp, text: &str) -> Result<()> {
    let hits = app.search(text);
    if hits.is_empty() {
        bail!("no results for {text:?}");
    }
    let first = &hits[0];
    println!(
        "invoke {} -> {:?}",
        first.title,
        first.target.as_deref().unwrap_or("")
    );
    let params = InvokeParams {
        item_id: first.id.clone(),
        action_id: "open".into(),
        text: text.into(),
    };
    let r = app.invoke(&params)?;
    println!("{r:?}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// 写一个 webview page 插件 fixture（manifest + 占位 index.html）。
    fn write_webview_plugin(dir: &Path, id: &str, keyword: &str) {
        fs::create_dir_all(dir).unwrap();
        let json = format!(
            r#"{{ "id": "{id}", "name": "T", "version": "0.1.0", "api_version": 2,
                 "runtime": "webview", "main": "index.html",
                 "features": [{{ "type": "keyword", "keyword": "{keyword}", "title": "T", "mode": "page" }}] }}"#
        );
        fs::write(dir.join("plugin.json"), json).unwrap();
        fs::write(dir.join("index.html"), "<html></html>").unwrap();
    }

    /// 空 app 索引的 HostApp：只测 search 的插件候选插入粘合，不建真实 Start Menu 索引。
    fn host_with_empty_index() -> HostApp {
        host_with_dirs(
            PathBuf::from("./__tests_plugins"),
            PathBuf::from("./__tests_data"),
        )
    }

    /// 同上，但插件/数据目录由调用方指定：会写 enabled/granted 状态的测试必须用
    /// 独立 data 目录——PluginState 持久化在 data/plugins-state.json，共享目录会让
    /// 测试间/多次运行互相污染（先跑的 disable 毒化后跑的 enabled 断言）。
    fn host_with_dirs(plugins: PathBuf, data: PathBuf) -> HostApp {
        HostApp {
            index: AppIndex::new(),
            plugins: PluginManager::with_dirs(plugins, data),
            config: HostConfig::default(),
            tray: None,
        }
    }

    /// 三个断言域共用：装载两个插件（关键字"翻译"+"翻译器"，后者是前者的真前缀超集）。
    fn host_with_translator_plugins(tmp: &Path) -> HostApp {
        write_webview_plugin(&tmp.join("com.spark.fy"), "com.spark.fy", "翻译");
        write_webview_plugin(&tmp.join("com.spark.trl"), "com.spark.trl", "翻译器");
        let mut app = host_with_empty_index();
        app.plugins.load_dev_dir(&tmp.join("com.spark.fy")).unwrap();
        app.plugins
            .load_dev_dir(&tmp.join("com.spark.trl"))
            .unwrap();
        app
    }

    /// native 纯应用插件 fixture（page 模型：无 commands，必有 page + 占位 html；
    /// 可选 features 关键字——native 的搜索框入口）。
    fn write_native_page_plugin(dir: &Path, id: &str) {
        write_native_page_plugin_kw(dir, id, None);
    }

    fn write_native_page_plugin_kw(dir: &Path, id: &str, keyword: Option<&str>) {
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

    fn api_params(plugin_id: &str, capability: &str, method: &str) -> PluginApiParams {
        PluginApiParams {
            plugin_id: plugin_id.into(),
            capability: capability.into(),
            method: method.into(),
            args: serde_json::Value::Null,
        }
    }

    #[test]
    fn plugin_api_rpc_returns_deferred_for_native_only() {
        // rpc 准备段：native 插件返回 deferred 请求（等待在锁外执行，本测试不执行）；
        // webview 插件无 exe 可转发，明确 UNAVAILABLE。
        let tmp = std::env::temp_dir().join("spark_host_rpc_deferred");
        let _ = std::fs::remove_dir_all(&tmp);
        let mut app = host_with_dirs(tmp.join("plugins"), tmp.join("data"));
        write_native_page_plugin(&tmp.join("com.spark.np"), "com.spark.np");
        app.plugins.load_dev_dir(&tmp.join("com.spark.np")).unwrap();

        let outcome = app
            .plugin_api(&api_params("com.spark.np", "rpc", "get_config"))
            .unwrap();
        assert!(matches!(outcome, PluginApiOutcome::NativePage(_)));

        // disabled 的 native 插件：准备段即 UNAVAILABLE（旧页面在途调用由关窗 shutdown 收尾）。
        let mut app2 = host_with_dirs(tmp.join("plugins"), tmp.join("data"));
        app2.plugins
            .load_dev_dir(&tmp.join("com.spark.np"))
            .unwrap();
        app2.plugins.set_enabled("com.spark.np", false).unwrap();
        let err2 = app2
            .plugin_api(&api_params("com.spark.np", "rpc", "get_config"))
            .unwrap_err();
        assert!(err2.to_string().contains("UNAVAILABLE"), "{err2}");

        // webview 插件无 exe 可转发：UNAVAILABLE（独立 data 目录，防 state 污染）。
        let mut app3 = host_with_dirs(tmp.join("plugins"), tmp.join("data3"));
        write_webview_plugin(&tmp.join("com.spark.wv"), "com.spark.wv", "wv");
        app3.plugins
            .load_dev_dir(&tmp.join("com.spark.wv"))
            .unwrap();
        let err3 = app3
            .plugin_api(&api_params("com.spark.wv", "rpc", "get_config"))
            .unwrap_err();
        assert!(err3.to_string().contains("UNAVAILABLE"), "{err3}");
    }

    #[test]
    fn plugin_page_closed_ignores_enabled_gate() {
        // 关窗关停不看 enabled：禁用流程先 toggle 再关窗，闸门会造成进程泄漏。
        // 关停对未运行进程是幂等 no-op——断言点是无闸门可调用且不 panic。
        let tmp = std::env::temp_dir().join("spark_host_page_closed");
        let _ = std::fs::remove_dir_all(&tmp);
        let mut app = host_with_dirs(tmp.join("plugins"), tmp.join("data"));
        write_native_page_plugin(&tmp.join("com.spark.np"), "com.spark.np");
        app.plugins.load_dev_dir(&tmp.join("com.spark.np")).unwrap();
        app.plugins.set_enabled("com.spark.np", false).unwrap();

        app.plugin_page_closed("com.spark.np"); // disabled 仍发出关停（fire-and-forget）
        app.plugin_page_closed("com.spark.absent"); // 未知 id 同样 no-op 不 panic
        app.plugin_page_closed("com.spark.wv-none"); // webview/未知 id 无操作
    }

    #[test]
    fn search_exact_hit_then_prefix_suggestion_ordering() {
        // 精确命中 + 前缀候选：精确(100)置顶，前缀候选紧随其后排在应用结果前面。
        let tmp = std::env::temp_dir().join("spark_host_kw_ordering");
        let _ = std::fs::remove_dir_all(&tmp);
        let mut app = host_with_translator_plugins(&tmp);

        let hits = app.search("翻译");
        assert_eq!(hits[0].id, "plugin:com.spark.fy:翻译");
        assert_eq!(hits[0].score, 100.0);
        assert_eq!(hits[1].id, "plugin:com.spark.trl:翻译器");
        assert_eq!(hits[1].score, 89.0);

        // 只有前缀（无精确命中）：前缀候选置顶
        let hits = app.search("翻");
        assert_eq!(hits[0].id, "plugin:com.spark.fy:翻译");
        assert_eq!(hits[0].score, 90.0);

        // 前缀带参进入参数阶段后不再给前缀建议：只剩精确路由的带参候选
        let hits = app.search("翻译 1");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "plugin:com.spark.fy:翻译");
    }

    #[test]
    fn search_prefix_order_stable_with_many_candidates() {
        // 多个前缀候选：按插入次序稳定排列，分数严格递减。
        let tmp = std::env::temp_dir().join("spark_host_kw_prefix_many");
        let _ = std::fs::remove_dir_all(&tmp);
        let mut app = host_with_translator_plugins(&tmp);
        write_webview_plugin(&tmp.join("com.spark.zn"), "com.spark.zn", "翻译指南");
        app.plugins.load_dev_dir(&tmp.join("com.spark.zn")).unwrap();

        let hits = app.search("翻译");
        assert_eq!(hits.len(), 3); // 精确 + 两个真前缀候选
        assert_eq!(hits[0].id, "plugin:com.spark.fy:翻译");
        assert_eq!(hits[1].id, "plugin:com.spark.trl:翻译器");
        assert_eq!(hits[2].id, "plugin:com.spark.zn:翻译指南");
        assert!(hits[0].score > hits[1].score && hits[1].score > hits[2].score);
    }

    #[test]
    fn search_native_keyword_candidate_opens_page() {
        // native 的 features（page 模式）关键字与 webview 同构地进搜索：精确命中
        // 置顶、target 指向同一套 plugin:page 开窗路由；无关键字 native 不产候选。
        let tmp = std::env::temp_dir().join("spark_host_kw_native");
        let _ = std::fs::remove_dir_all(&tmp);
        let mut app = host_with_dirs(tmp.join("plugins"), tmp.join("data"));
        write_native_page_plugin_kw(&tmp.join("com.spark.np"), "com.spark.np", Some("echo"));
        write_native_page_plugin(&tmp.join("com.spark.bare"), "com.spark.bare");
        app.plugins.load_dev_dir(&tmp.join("com.spark.np")).unwrap();
        app.plugins
            .load_dev_dir(&tmp.join("com.spark.bare"))
            .unwrap();

        let hits = app.search("echo");
        assert_eq!(hits[0].id, "plugin:com.spark.np:echo");
        assert_eq!(hits[0].score, 100.0);
        assert_eq!(hits[0].target.as_deref(), Some("plugin:page:com.spark.np"));
        assert_eq!(hits[0].source, Source::Plugin);

        // 前缀建议同样适用。
        let hits = app.search("ech");
        assert_eq!(hits[0].id, "plugin:com.spark.np:echo");

        // 无 features 的 native：不产候选（页面走卡片「打开」）。
        assert!(app
            .search("bare")
            .iter()
            .all(|c| c.source != Source::Plugin));

        // 带参候选：input 语义与 webview 一致（UI 拆 "echo hi" → input="hi"）。
        let hits = app.search("echo hi");
        assert_eq!(hits[0].id, "plugin:com.spark.np:echo");
    }
}
