//! 插件启用/授权状态持久化（`<data_dir>/plugins-state.json`）。
//!
//! 独立于 `config.toml`：核心设置保持用户可手编 TOML，插件状态是应用管理的元数据。

use serde::{Deserialize, Serialize};
use spark_core::data_dir;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginState {
    #[serde(default)]
    pub enabled: BTreeMap<String, bool>,
    #[serde(default)]
    pub granted: BTreeMap<String, Vec<String>>,
}

impl PluginState {
    pub fn path() -> PathBuf {
        data_dir().join("plugins-state.json")
    }

    /// 读取状态；缺失或损坏时返回默认（不抛错——状态可重建）。
    pub fn load() -> Self {
        Self::load_at(&Self::path())
    }

    /// 从指定路径读取（测试/注入用）。
    pub fn load_at(path: &std::path::Path) -> Self {
        match fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// 原子保存到默认全局路径。
    pub fn save(&self) -> io::Result<()> {
        self.save_to(&Self::path())
    }

    /// 原子保存到指定路径：写临时文件再 rename（与 config 同款，防中断损坏）。
    pub fn save_to(&self, path: &std::path::Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let text =
            serde_json::to_vec_pretty(self).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, &text)?;
        if path.exists() {
            // Windows 上目标已存在时直接 rename 会失败，先备份再换。
            let bak = path.with_extension("json.bak");
            let _ = fs::remove_file(&bak);
            fs::rename(path, &bak)?;
            match fs::rename(&tmp, path) {
                Ok(()) => {
                    let _ = fs::remove_file(&bak);
                }
                Err(e) => {
                    let _ = fs::rename(&bak, path);
                    return Err(e);
                }
            }
        } else {
            fs::rename(&tmp, path)?;
        }
        Ok(())
    }

    pub fn enabled_of(&self, id: &str) -> bool {
        self.enabled.get(id).copied().unwrap_or(true)
    }

    pub fn granted_of(&self, id: &str) -> Vec<String> {
        self.granted.get(id).cloned().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_enabled_and_empty_grants() {
        let s = PluginState::default();
        assert!(s.enabled_of("com.spark.any"));
        assert!(s.granted_of("com.spark.any").is_empty());
    }

    #[test]
    fn roundtrip_preserves_state() {
        let mut s = PluginState::default();
        s.enabled.insert("com.spark.x".into(), false);
        s.granted
            .insert("com.spark.x".into(), vec!["clipboard".into()]);
        let json = serde_json::to_string(&s).unwrap();
        let back: PluginState = serde_json::from_str(&json).unwrap();
        assert!(!back.enabled_of("com.spark.x"));
        assert_eq!(
            back.granted_of("com.spark.x"),
            vec!["clipboard".to_string()]
        );
    }
}
