//! Plugin discovery and manifest handling (process spawn comes later).

mod error;
mod manifest;

pub use error::PluginError;
pub use manifest::{PluginCommand, PluginManifest, PluginRuntime};

use std::fs;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

#[derive(Debug, Default)]
pub struct PluginManager {
    plugins: Vec<LoadedPlugin>,
}

#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    pub manifest: PluginManifest,
    pub root: PathBuf,
}

impl PluginManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn plugins(&self) -> &[LoadedPlugin] {
        &self.plugins
    }

    /// Scan `dir` for subfolders containing `plugin.json`.
    pub fn scan_dir(&mut self, dir: &Path) -> Result<usize, PluginError> {
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
                    info!(id = %manifest.id, path = %path.display(), "loaded plugin manifest");
                    self.plugins.push(LoadedPlugin {
                        manifest,
                        root: path,
                    });
                    n += 1;
                }
                Err(e) => warn!(?e, path = %manifest_path.display(), "skip plugin"),
            }
        }
        Ok(n)
    }
}
