use serde::{Deserialize, Serialize};
use spark_core::{config_path, ensure_data_dir};
use spark_ipc::TrustedPubkeyEntry;
use std::path::PathBuf;
use std::{fs, io};
use tracing::info;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    #[serde(default = "default_hotkey")]
    pub hotkey_toggle: String,
    #[serde(default = "default_max_results")]
    pub max_results: u32,
    #[serde(default)]
    pub plugins_dir: Option<PathBuf>,
    // 失焦隐藏 / 执行后隐藏已固化为始终开启的默认行为（UI 不再提供开关、不再推送），
    // host 不消费这两个值；字段仅为兼容旧 config.toml（deny_unknown_fields 会拒载
    // 含未知字段的文件，删除会导致老用户整份配置回退默认）而保留。
    #[serde(default = "default_true")]
    pub hide_on_focus_lost: bool,
    #[serde(default = "default_true")]
    pub hide_on_execute: bool,
    #[serde(default)]
    pub launch_on_startup: bool,
    #[serde(default = "default_true")]
    pub hotkey_enabled: bool,
    /// 严格模式（规范 §12.2 3.2，默认关）：开启后仅安装带有效签名的插件
    /// （官方或用户已信任的三方签名），无签名拒装。
    #[serde(default)]
    pub strict_mode: bool,
    /// 用户导入的"受信任开发者"三方公钥表（规范 §10）。host 验签时合并进
    /// 运行时可信表，命中即 `SignState::ThirdParty`；恒不能产生"官方"。
    #[serde(default)]
    pub trusted_pubkeys: Vec<TrustedPubkeyEntry>,
    /// 插件市场仓库 URL 列表（规范 §6 / §7）。空列表 = 仅官方仓库。
    #[serde(default)]
    pub plugin_registry_urls: Vec<String>,
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
            strict_mode: false,
            trusted_pubkeys: Vec::new(),
            plugin_registry_urls: Vec::new(),
        }
    }
}

impl HostConfig {
    pub fn load_or_default() -> Self {
        let _ = ensure_data_dir();
        let path = config_path();
        match load_valid_config(&path) {
            Some(c) => {
                info!(path = %path.display(), "loaded config");
                c
            }
            None => {
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
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, text)?;
        replace_file(&tmp, &path)?;
        Ok(())
    }
}

fn load_valid_config(path: &std::path::Path) -> Option<HostConfig> {
    if let Ok(text) = fs::read_to_string(path) {
        if let Ok(config) = toml::from_str(&text) {
            let _ = fs::remove_file(path.with_extension("toml.tmp"));
            let _ = fs::remove_file(path.with_extension("toml.bak"));
            return Some(config);
        }
    }
    for candidate in [
        path.with_extension("toml.bak"),
        path.with_extension("toml.tmp"),
    ] {
        if let Ok(text) = fs::read_to_string(&candidate) {
            if let Ok(config) = toml::from_str::<HostConfig>(&text) {
                let had_target = path.exists();
                let displaced = path.with_extension("toml.corrupt");
                if had_target && fs::rename(path, &displaced).is_err() {
                    continue;
                }
                match fs::rename(&candidate, path) {
                    Ok(()) => {
                        if had_target {
                            let _ = fs::remove_file(&displaced);
                        }
                        info!(path = %path.display(), "recovered config after interrupted save");
                        return Some(config);
                    }
                    Err(_) => {
                        if had_target {
                            let _ = fs::rename(&displaced, path);
                        }
                    }
                }
            }
        }
    }
    None
}

fn replace_file(tmp: &std::path::Path, target: &std::path::Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        if !target.exists() {
            return fs::rename(tmp, target);
        }
        let backup = target.with_extension("toml.bak");
        let _ = fs::remove_file(&backup);
        fs::rename(target, &backup)?;
        match fs::rename(tmp, target) {
            Ok(()) => {
                let _ = fs::remove_file(backup);
                Ok(())
            }
            Err(e) => {
                let remove_result = fs::remove_file(target);
                let restore_result = fs::rename(backup, target);
                if let Err(restore_error) = restore_result {
                    return Err(io::Error::new(
                        restore_error.kind(),
                        format!("replace failed: {e}; restore failed: {restore_error}; remove target: {remove_result:?}"),
                    ));
                }
                Err(e)
            }
        }
    }
    #[cfg(not(windows))]
    {
        fs::rename(tmp, target)
    }
}
