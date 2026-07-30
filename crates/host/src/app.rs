use crate::config::HostConfig;
use crate::shell;
use anyhow::{bail, Result};
use spark_core::{ensure_data_dir, Candidate, Query};
use spark_index::{AppIndex, SearchIndex};
use spark_ipc::{InvokeParams, InvokeResult};
use spark_plugin_manager::PluginManager;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

pub struct HostApp {
    index: AppIndex,
    plugins: PluginManager,
    pub config: HostConfig,
}

impl HostApp {
    pub fn bootstrap(extra_plugins: Option<&Path>) -> Result<Self> {
        let _ = ensure_data_dir()?;
        let config = HostConfig::load_or_default();
        info!("building app index from Start Menu…");
        let index = AppIndex::with_seed_fallback();
        info!(apps = index.len(), "app index ready");

        let mut plugins = PluginManager::new();
        let default_plugins = Path::new("plugins");
        if default_plugins.is_dir() {
            let n = plugins.scan_dir(default_plugins)?;
            info!(count = n, "scanned ./plugins");
        }
        if let Some(dir) = extra_plugins.or(config.plugins_dir.as_deref()) {
            if dir.is_dir() {
                let n = plugins.scan_dir(dir)?;
                info!(count = n, path = %dir.display(), "scanned plugins dir");
            }
        }

        Ok(Self {
            index,
            plugins,
            config,
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

        match shell::invoke_action(item.target.as_deref(), action) {
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
