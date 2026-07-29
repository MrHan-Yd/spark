use crate::PluginError;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginRuntime {
    Native,
    Wasm,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginCommand {
    pub name: String,
    pub title: String,
    #[serde(default)]
    pub subtitle: Option<String>,
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default)]
    pub prefix: Option<String>,
}

fn default_mode() -> String {
    "list".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub api_version: u32,
    pub main: String,
    pub runtime: PluginRuntime,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub commands: Vec<PluginCommand>,
    #[serde(default)]
    pub permissions: Vec<String>,
}

impl PluginManifest {
    pub fn load(path: &Path) -> Result<Self, PluginError> {
        let raw = fs::read_to_string(path)?;
        let m: Self = serde_json::from_str(&raw)?;
        m.validate()?;
        Ok(m)
    }

    pub fn validate(&self) -> Result<(), PluginError> {
        if self.id.is_empty() || !self.id.contains('.') {
            return Err(PluginError::Manifest(
                "id must be reverse-domain style".into(),
            ));
        }
        if self.commands.is_empty() {
            return Err(PluginError::Manifest("commands must not be empty".into()));
        }
        if self.main.is_empty() {
            return Err(PluginError::Manifest("main is required".into()));
        }
        Ok(())
    }
}
