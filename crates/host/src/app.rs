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
    KeyKind, KeywordMatch, PluginInfo, PluginInstallOutcome, PluginManager, PluginOpenInfo,
    TrustedKey,
};
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

pub struct HostApp {
    /// 应用索引（index_watch 后台重建后 swap_memory 原子换入）。
    pub index: AppIndex,
    plugins: PluginManager,
    pub config: HostConfig,
    /// 托盘图标（win_loop 创建后注入；用于右下角气泡提示）
    pub tray: Option<TrayIcon>,
}

impl HostApp {
    pub fn bootstrap(extra_plugins: Option<&Path>) -> Result<Self> {
        let _ = ensure_data_dir()?;
        let config = HostConfig::load_or_default();
        info!("building app index from Start Menu…");
        let index = AppIndex::with_seed_fallback();
        info!(apps = index.len(), "app index ready");

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

    pub fn search(&mut self, text: &str) -> Vec<Candidate> {
        let q = Query {
            text: text.into(),
            limit: self.config.max_results,
        };
        let mut hits = self.index.search(&q);
        // 插件关键字路由：命中 webview 插件的 keyword feature 时，在结果最前
        // 插入一个 page-mode 候选项，UI 见 target="plugin:page:<id>" 即开插件窗口。
        if let Some(m) = self.plugins.find_keyword_match(text) {
            if let Some(cand) = self.build_plugin_candidate(&m) {
                hits.insert(0, cand);
            }
        }
        // native 插件（mode:list）：命中关键字前缀时，把插件返回的结果项并入列表。
        // 注：native_query 是阻塞 RPC（带超时），在 host 锁内调用；native 插件为重型
        // 组件，超时/崩溃自动降级为空结果，不阻断搜索主流程。
        if let Some(m) = self.plugins.find_native_match(text) {
            if self.plugins.native_plugin(&m.plugin_id).is_some() {
                match self
                    .plugins
                    .native_query(&m.plugin_id, &m.input, self.config.max_results)
                {
                    Ok(result) if !result.items.is_empty() => {
                        for item in result.items {
                            hits.push(item);
                        }
                    }
                    Ok(_) => {}
                    Err(e) => warn!(?e, id = %m.plugin_id, "native query returned error"),
                }
            }
        }
        hits
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

        // native 插件结果项：plugin_id 命中 native 插件时走 native_invoke，
        // 不经 shell/索引（native 项 target 通常为空）。
        if let Some(pid) = item.plugin_id.as_deref() {
            if self.plugins.native_plugin(pid).is_some() {
                let result = self.plugins.native_invoke(pid, params.clone())?;
                return Ok(result);
            }
        }

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

    pub fn index_len(&self) -> usize {
        self.index.len()
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

    /// 更换插件目录并迁移；同步写回 config.plugins_dir 并保存。
    pub fn plugin_set_dir(&mut self, path: &str, migrate: bool) -> Result<()> {
        let new_dir = PathBuf::from(path);
        self.plugins.set_dir(&new_dir, migrate)?;
        self.config.plugins_dir = Some(new_dir);
        self.config.save()?;
        Ok(())
    }

    pub fn plugins_dir(&self) -> &Path {
        self.plugins.plugins_dir()
    }

    /// `spark.*` 特权能力桥：校验声明+授权后执行 clipboard/notify/db。
    /// 返回值的 data 字段直接回传给插件页 JS。
    pub fn plugin_api(&mut self, params: &PluginApiParams) -> Result<serde_json::Value> {
        let declared = self.plugins.declared_permissions(&params.plugin_id);
        let granted = self.plugins.granted(&params.plugin_id);
        // 权限必须清单声明且用户授权（db 默认开放）。
        let has =
            |perm: &str| declared.iter().any(|p| p == perm) && granted.iter().any(|p| p == perm);

        match params.capability.as_str() {
            "db" => Ok(self.plugins.plugin_db(
                &params.plugin_id,
                &params.method,
                params.args.clone(),
            )?),
            "clipboard" => {
                if !has("clipboard") {
                    bail!("PERMISSION_DENIED: clipboard");
                }
                self.plugin_api_clipboard(&params.method, &params.args)
            }
            "notify" => {
                if !has("notify") {
                    bail!("PERMISSION_DENIED: notify");
                }
                self.plugin_api_notify(&params.args)
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
