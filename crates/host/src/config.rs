use serde::{Deserialize, Serialize};
use spark_core::{config_path, ensure_data_dir};
use std::path::PathBuf;
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostConfig {
    #[serde(default = "default_hotkey")]
    pub hotkey_toggle: String,
    #[serde(default = "default_max_results")]
    pub max_results: u32,
    #[serde(default)]
    pub plugins_dir: Option<PathBuf>,
    #[serde(default = "default_true")]
    pub hide_on_focus_lost: bool,
    #[serde(default = "default_true")]
    pub hide_on_execute: bool,
    #[serde(default)]
    pub launch_on_startup: bool,
    #[serde(default = "default_true")]
    pub hotkey_enabled: bool,
}

fn default_hotkey() -> String {
    "Alt+Space".into()
}
fn default_max_results() -> u32 {
    50
}
fn default_true() -> bool {
    true
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            hotkey_toggle: default_hotkey(),
            max_results: default_max_results(),
            plugins_dir: None,
            hide_on_focus_lost: true,
            hide_on_execute: true,
            launch_on_startup: false,
            hotkey_enabled: true,
        }
    }
}

impl HostConfig {
    pub fn load_or_default() -> Self {
        let _ = ensure_data_dir();
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str(&text) {
                Ok(c) => {
                    info!(path = %path.display(), "loaded config");
                    c
                }
                Err(e) => {
                    warn!(?e, "config parse failed; using defaults");
                    Self::default()
                }
            },
            Err(_) => {
                let c = Self::default();
                let _ = c.save();
                c
            }
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let _ = ensure_data_dir();
        let path = config_path();
        let text = toml::to_string_pretty(self)?;
        std::fs::write(&path, text)?;
        Ok(())
    }
}
