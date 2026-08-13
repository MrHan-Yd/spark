//! Data / config directory resolution.

use std::path::PathBuf;

/// `%APPDATA%/Spark` or portable `./data` next to exe when present.
pub fn data_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let portable = dir.join("data");
            if portable.is_dir() {
                return portable;
            }
        }
    }
    dirs_next_appdata()
        .map(|p| p.join("Spark"))
        .unwrap_or_else(|| PathBuf::from("Spark"))
}

pub fn config_path() -> PathBuf {
    data_dir().join("config.toml")
}

pub fn history_path() -> PathBuf {
    data_dir().join("history.json")
}

pub fn ensure_data_dir() -> std::io::Result<PathBuf> {
    let d = data_dir();
    std::fs::create_dir_all(&d)?;
    std::fs::create_dir_all(d.join("logs"))?;
    std::fs::create_dir_all(d.join("cache"))?;
    // 插件目录固定在应用安装目录（{app}\plugins），不在此处创建，避免误导。
    Ok(d)
}

fn dirs_next_appdata() -> Option<PathBuf> {
    // Prefer APPDATA (Roaming) on Windows.
    if let Ok(p) = std::env::var("APPDATA") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    if let Ok(home) = std::env::var("USERPROFILE") {
        return Some(PathBuf::from(home).join("AppData").join("Roaming"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_nonempty() {
        assert!(!data_dir().as_os_str().is_empty());
    }
}
