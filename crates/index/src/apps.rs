//! Enumerate Start Menu shortcuts (.lnk) as app candidates.

use spark_core::{Action, Candidate, Source};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tracing::{debug, info};

/// Scan common Start Menu roots and return app candidates.
pub fn enumerate_start_menu_apps() -> Vec<Candidate> {
    let mut roots = Vec::new();
    if let Ok(appdata) = std::env::var("APPDATA") {
        roots.push(PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    if let Ok(program_data) = std::env::var("ProgramData") {
        roots.push(PathBuf::from(program_data).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    // Common extra locations
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        roots.push(PathBuf::from(local).join(r"Microsoft\Windows\Start Menu\Programs"));
    }

    let mut seen = HashSet::new();
    let mut items = Vec::new();

    for root in roots {
        if !root.is_dir() {
            continue;
        }
        collect_lnks(&root, &root, &mut seen, &mut items);
    }

    // Always include a few well-known system entries if missing
    push_system_builtins(&mut seen, &mut items);

    info!(count = items.len(), "enumerated start-menu apps");
    items
}

fn collect_lnks(root: &Path, dir: &Path, seen: &mut HashSet<String>, out: &mut Vec<Candidate>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            debug!(path = %dir.display(), ?e, "skip dir");
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Skip installer noise folders lightly
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();
            if name == "startup" || name.starts_with('.') {
                continue;
            }
            collect_lnks(root, &path, seen, out);
            continue;
        }

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if ext != "lnk" && ext != "exe" && ext != "url" {
            continue;
        }

        let title = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Unknown")
            .to_string();

        // Skip uninstallers / help dumps
        let lower = title.to_lowercase();
        if lower.contains("uninstall")
            || lower.contains("卸载")
            || lower.starts_with("unins")
            || lower.contains("readme")
            || lower.contains("release notes")
            || lower.contains("help") && (lower.contains("online") || lower.ends_with(" help"))
        {
            continue;
        }

        let target = path.to_string_lossy().to_string();
        let id = app_id_for(&target);
        if !seen.insert(id.clone()) {
            continue;
        }

        let subtitle = relative_folder(root, &path);
        let mtime_boost = path
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| (d.as_secs() % 10_000) as f32 * 0.00001)
            .unwrap_or(0.0);

        out.push(Candidate {
            id,
            title,
            subtitle: Some(subtitle.unwrap_or_else(|| "应用程序".into())),
            target: Some(target.clone()),
            icon: Some(target),
            score: 0.85 + mtime_boost,
            source: Source::App,
            actions: vec![Action::open_default(), Action::reveal()],
            plugin_id: None,
        });
    }
}

fn relative_folder(root: &Path, file: &Path) -> Option<String> {
    let parent = file.parent()?;
    let rel = parent.strip_prefix(root).ok()?;
    let s = rel.to_string_lossy();
    if s.is_empty() {
        Some("开始菜单".into())
    } else {
        Some(s.replace('\\', " / "))
    }
}

fn app_id_for(target: &str) -> String {
    // Stable-ish id from lowercase path
    let key = target.to_lowercase().replace('/', "\\");
    format!("app:{}", simple_hash(&key))
}

fn simple_hash(s: &str) -> String {
    // FNV-1a 64 → hex (no extra deps)
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn push_system_builtins(seen: &mut HashSet<String>, out: &mut Vec<Candidate>) {
    let builtins: &[(&str, &str, &str)] = &[
        ("sys.explorer", "文件资源管理器", r"C:\Windows\explorer.exe"),
        ("sys.cmd", "命令提示符", r"C:\Windows\System32\cmd.exe"),
        (
            "sys.powershell",
            "Windows PowerShell",
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
        ),
        ("sys.notepad", "记事本", r"C:\Windows\System32\notepad.exe"),
        ("sys.calc", "计算器", r"C:\Windows\System32\calc.exe"),
    ];

    for (id, title, path) in builtins {
        if !Path::new(path).exists() {
            continue;
        }
        if !seen.insert((*id).into()) {
            continue;
        }
        out.push(Candidate {
            id: (*id).into(),
            title: (*title).into(),
            subtitle: Some("系统".into()),
            target: Some((*path).into()),
            icon: Some((*path).into()),
            score: 0.9,
            source: Source::App,
            actions: vec![Action::open_default(), Action::reveal()],
            plugin_id: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_stable() {
        assert_eq!(simple_hash("a"), simple_hash("a"));
        assert_ne!(simple_hash("a"), simple_hash("b"));
    }

    #[test]
    fn enumerate_runs() {
        // Should not panic even if Start Menu empty in CI
        let _ = enumerate_start_menu_apps();
    }
}
