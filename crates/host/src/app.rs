use crate::builtins;
use crate::config::HostConfig;
use crate::shell;
use crate::tray::TrayIcon;
use anyhow::{bail, Result};
use spark_core::{ensure_data_dir, Candidate, Query};
use spark_index::{AppIndex, SearchIndex};
use spark_ipc::{InvokeParams, InvokeResult};
use spark_plugin_manager::PluginManager;
use std::path::Path;
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

        let mut plugins = PluginManager::new();
        // 插件只从应用安装目录（host 同目录）的 plugins 文件夹加载，安装位置无关；
        // 开发/测试用 `--plugins-dir` 或配置 `plugins_dir` 显式指定（dev_host.ps1 已传）。
        if let Some(dir) = crate::exe_dir().map(|d| d.join("plugins")) {
            if dir.is_dir() {
                let n = plugins.scan_dir(&dir)?;
                info!(count = n, path = %dir.display(), "scanned plugins");
            }
        }
        if let Some(dir) = extra_plugins.or(config.plugins_dir.as_deref()) {
            // 配置/CLI 里写相对路径时按 host 目录解析，避免依赖 CWD。
            let dir = match crate::exe_dir() {
                Some(base) if dir.is_relative() => base.join(dir),
                _ => dir.to_path_buf(),
            };
            if dir.is_dir() {
                let n = plugins.scan_dir(&dir)?;
                info!(count = n, path = %dir.display(), "scanned plugins dir");
            }
        }

        Ok(Self {
            index,
            plugins,
            config,
            tray: None,
        })
    }

    pub fn search(&self, text: &str) -> Vec<Candidate> {
        let q = Query {
            text: text.into(),
            limit: self.config.max_results,
        };
        self.index.search(&q)
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

    pub fn index_len(&self) -> usize {
        self.index.len()
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
        self.config.hotkey_enabled = enabled;
        let _ = self.config.save();
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
