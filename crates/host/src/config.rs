use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostConfig {
    pub hotkey_toggle: String,
    pub max_results: u32,
    pub plugins_dir: Option<PathBuf>,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            hotkey_toggle: "Alt+Space".into(),
            max_results: 50,
            plugins_dir: None,
        }
    }
}
