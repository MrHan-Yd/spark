use anyhow::Result;
use spark_core::{Candidate, Query};
use spark_index::{MemoryIndex, SearchIndex};
use spark_plugin_manager::PluginManager;
use std::path::Path;
use tracing::info;

pub struct HostApp {
    index: MemoryIndex,
    plugins: PluginManager,
    pub config: crate::config::HostConfig,
}

impl HostApp {
    pub fn bootstrap(extra_plugins: Option<&Path>) -> Result<Self> {
        let config = crate::config::HostConfig::default();
        let index = MemoryIndex::with_seed_apps();
        let mut plugins = PluginManager::new();

        let default_plugins = Path::new("plugins");
        if default_plugins.is_dir() {
            let n = plugins.scan_dir(default_plugins)?;
            info!(count = n, "scanned ./plugins");
        }
        if let Some(dir) = extra_plugins {
            let n = plugins.scan_dir(dir)?;
            info!(count = n, path = %dir.display(), "scanned extra plugins");
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

    pub fn index_len(&self) -> usize {
        self.index.len()
    }

    pub fn plugin_count(&self) -> usize {
        self.plugins.plugins().len()
    }
}
