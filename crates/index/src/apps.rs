//! Enumerate Start Menu shortcuts (.lnk) as app candidates.
//!
//! Shortcuts pointing at the same exe are merged into one app row (uTools
//! style): the "cleanest" shortcut (no arguments) becomes the primary row with
//! the real exe as target/icon, and argument-carrying shortcuts ("Chrome 无痕
//! 模式") become secondary actions on that row instead of separate rows.

use crate::lnk::{resolve_lnk, LnkInfo};
use spark_core::{Action, Candidate, Source};
use std::collections::{HashMap, HashSet};
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

    let mut collected = Vec::new();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        collect_entries(&root, &root, &mut collected);
    }

    let mut seen = HashSet::new();
    let mut items = merge_apps(collected, &mut seen);

    // Always include a few well-known system entries if missing
    push_system_builtins(&mut seen, &mut items);

    info!(count = items.len(), "enumerated start-menu apps");
    items
}

/// A raw Start Menu entry, before merging by target exe.
struct Collected {
    /// The .lnk / .exe / .url path itself.
    path: PathBuf,
    /// Display title (file stem).
    title: String,
    /// Folder relative to the scan root (for the subtitle).
    folder: Option<String>,
    /// Modified-time ranking boost.
    mtime_boost: f32,
    /// Canonical target exe when the shortcut resolved successfully.
    exe: Option<String>,
    /// Command-line arguments of the shortcut.
    args: Option<String>,
}

fn collect_entries(root: &Path, dir: &Path, out: &mut Vec<Collected>) {
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
            collect_entries(root, &path, out);
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
        if is_noise_title(&title) {
            continue;
        }

        let folder = relative_folder(root, &path);
        let mtime_boost = path
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| (d.as_secs() % 10_000) as f32 * 0.00001)
            .unwrap_or(0.0);

        let mut exe = None;
        let mut args = None;
        if ext == "lnk" {
            if let Some(info) = resolve_lnk(&path) {
                match classify_lnk(&info) {
                    LnkOutcome::Merge { exe: e, args: a } => {
                        exe = Some(e);
                        args = a;
                    }
                    // Broken shortcut (target file gone): don't surface it
                    LnkOutcome::Drop => continue,
                    // Special target (CLSID / shell: / protocol): keep the raw
                    // shortcut as its own row.
                    LnkOutcome::KeepRaw => {}
                }
            }
            // Resolution failed → fall through with exe=None: keep the raw
            // shortcut as its own row rather than dropping the app.
        } else if ext == "exe" {
            exe = path
                .canonicalize()
                .ok()
                .map(|c| normalize_path(&c.to_string_lossy()));
        }
        // .url stays exe=None: a standalone row, never merged.

        out.push(Collected {
            path,
            title,
            folder,
            mtime_boost,
            exe,
            args,
        });
    }
}

/// What to do with a successfully parsed shortcut.
enum LnkOutcome {
    /// Target is a live file: merge rows by this canonical exe path.
    Merge { exe: String, args: Option<String> },
    /// Target is not a plain file path (CLSID / shell: / folder): keep raw.
    KeepRaw,
    /// Target file is gone (dead shortcut): drop the row.
    Drop,
}

/// Classify a resolved shortcut target.
fn classify_lnk(info: &LnkInfo) -> LnkOutcome {
    let Some(target) = &info.target else {
        return LnkOutcome::KeepRaw;
    };
    // Plain file paths look like `C:\...` or `\\server\...`; everything else
    // (CLSID `::{...}`, `shell:...`, protocols) must stay as a raw row.
    let looks_file = target.starts_with(r"\\") || target.contains(":\\");
    if !looks_file {
        return LnkOutcome::KeepRaw;
    }
    let target_path = PathBuf::from(target);
    if target_path.is_dir() {
        return LnkOutcome::KeepRaw; // folder shortcut
    }
    if !target_path.is_file() {
        return LnkOutcome::Drop; // dead shortcut
    }
    let Ok(canon) = target_path.canonicalize() else {
        return LnkOutcome::KeepRaw;
    };
    let canon_str = normalize_path(&canon.to_string_lossy());
    if is_noise_target(&canon_str) {
        return LnkOutcome::Drop;
    }
    LnkOutcome::Merge {
        exe: canon_str,
        args: info.args.clone(),
    }
}

/// `canonicalize` may return `\\?\`-prefixed extended paths; strip the prefix
/// (and UNC form) so targets stay in plain `C:\...` shape everywhere.
fn normalize_path(s: &str) -> String {
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s.to_string()
    }
}

/// Merge collected entries by target exe; unresolvable entries stay as-is.
fn merge_apps(collected: Vec<Collected>, seen: &mut HashSet<String>) -> Vec<Candidate> {
    let mut groups: HashMap<String, Vec<Collected>> = HashMap::new();
    let mut singles: Vec<Collected> = Vec::new();
    for c in collected {
        match &c.exe {
            Some(exe) => groups.entry(exe.to_lowercase()).or_default().push(c),
            None => singles.push(c),
        }
    }

    let mut items = Vec::new();
    for c in singles {
        push_raw_row(c, seen, &mut items);
    }
    for (_key, mut group) in groups {
        // Primary = the "cleanest" shortcut: no args, then short args,
        // then short title. Everything else becomes a secondary action.
        group.sort_by(|a, b| {
            let a_args = a.args.as_ref().map(|s| s.len()).unwrap_or(0);
            let b_args = b.args.as_ref().map(|s| s.len()).unwrap_or(0);
            a_args
                .cmp(&b_args)
                .then_with(|| a.title.len().cmp(&b.title.len()))
        });
        let primary = group.remove(0);
        let exe = primary.exe.clone().unwrap_or_default();
        let id = app_id_for(&exe);
        if !seen.insert(id.clone()) {
            continue;
        }

        let mut actions = vec![Action::open_default()];
        actions.extend(group.into_iter().map(|c| Action {
            id: format!(
                "alt:{}",
                simple_hash(&c.path.to_string_lossy().to_lowercase())
            ),
            title: c.title,
            is_default: false,
            target: Some(c.path.to_string_lossy().to_string()),
        }));
        actions.push(Action::reveal());

        items.push(Candidate {
            id,
            title: primary.title,
            subtitle: Some(primary.folder.unwrap_or_else(|| "应用程序".into())),
            target: Some(exe.clone()),
            icon: Some(exe),
            score: 0.85 + primary.mtime_boost,
            source: Source::App,
            actions,
            plugin_id: None,
        });
    }
    items
}

/// A standalone row for entries without a resolved exe (.url, failed .lnk).
fn push_raw_row(c: Collected, seen: &mut HashSet<String>, out: &mut Vec<Candidate>) {
    let target = c.path.to_string_lossy().to_string();
    let id = app_id_for(&target);
    if !seen.insert(id.clone()) {
        return;
    }
    out.push(Candidate {
        id,
        title: c.title,
        subtitle: Some(c.folder.unwrap_or_else(|| "应用程序".into())),
        target: Some(target.clone()),
        icon: Some(target),
        score: 0.85 + c.mtime_boost,
        source: Source::App,
        actions: vec![Action::open_default(), Action::reveal()],
        plugin_id: None,
    });
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

/// Skip uninstallers / help dumps by shortcut title.
fn is_noise_title(title: &str) -> bool {
    let lower = title.to_lowercase();
    lower.contains("uninstall")
        || lower.contains("卸载")
        || lower.starts_with("unins")
        || lower.contains("readme")
        || lower.contains("release notes")
        || lower.contains("help") && (lower.contains("online") || lower.ends_with(" help"))
}

/// Skip uninstaller executables that masquerade under a clean shortcut name.
fn is_noise_target(exe: &str) -> bool {
    let name = Path::new(exe)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    name.starts_with("unins") || name.contains("uninstall") || name.contains("卸载")
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

    fn collected(path: &str, title: &str, exe: Option<&str>, args: Option<&str>) -> Collected {
        Collected {
            path: PathBuf::from(path),
            title: title.into(),
            folder: None,
            mtime_boost: 0.0,
            exe: exe.map(|s| s.into()),
            args: args.map(|s| s.into()),
        }
    }

    #[test]
    fn merges_same_exe_shortcuts() {
        let chrome = r"C:\Program Files\Google\Chrome\Application\chrome.exe";
        let input = vec![
            collected(r"C:\sm\Chrome.lnk", "Google Chrome", Some(chrome), None),
            collected(
                r"C:\sm\Chrome 无痕模式.lnk",
                "Chrome 无痕模式",
                Some(chrome),
                Some("--incognito"),
            ),
            collected(
                r"C:\sm\Chrome 配置2.lnk",
                "Chrome 配置2",
                Some(chrome),
                Some("--user-data-dir=C:\\x"),
            ),
        ];
        let mut seen = HashSet::new();
        let items = merge_apps(input, &mut seen);
        assert_eq!(items.len(), 1);
        let m = &items[0];
        assert_eq!(m.title, "Google Chrome");
        assert_eq!(m.target.as_deref(), Some(chrome));
        assert_eq!(m.icon.as_deref(), Some(chrome));
        // open + 2 alt actions + reveal
        assert_eq!(m.actions.len(), 4);
        assert_eq!(m.actions[1].title, "Chrome 无痕模式");
        assert_eq!(m.actions[2].title, "Chrome 配置2");
        assert!(m.actions[1]
            .target
            .as_deref()
            .unwrap()
            .ends_with("无痕模式.lnk"));
        assert!(!m.actions[1].is_default);
    }

    #[test]
    fn distinct_exes_stay_separate() {
        let input = vec![
            collected(r"C:\sm\A.lnk", "App A", Some(r"C:\a\a.exe"), None),
            collected(r"C:\sm\B.lnk", "App B", Some(r"C:\b\b.exe"), None),
        ];
        let mut seen = HashSet::new();
        let items = merge_apps(input, &mut seen);
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn unresolved_lnk_stays_single_row() {
        let input = vec![collected(r"C:\sm\broken.lnk", "Broken", None, None)];
        let mut seen = HashSet::new();
        let items = merge_apps(input, &mut seen);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].target.as_deref(), Some(r"C:\sm\broken.lnk"));
    }

    #[test]
    fn same_exe_deduped_across_singletons() {
        let exe = r"C:\tools\dup.exe";
        let input = vec![
            collected(r"C:\sm\Dup.lnk", "Dup", Some(exe), None),
            collected(r"C:\sm\Dup2.lnk", "Dup2", Some(exe), None),
        ];
        let mut seen = HashSet::new();
        let items = merge_apps(input, &mut seen);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].actions.len(), 3); // open + alt + reveal
    }

    #[test]
    fn noise_title_filtered() {
        assert!(is_noise_title("Uninstall Google Chrome"));
        assert!(is_noise_title("卸载微信"));
        assert!(is_noise_title("unins000"));
        assert!(!is_noise_title("Google Chrome"));
    }

    #[test]
    fn noise_target_filtered() {
        assert!(is_noise_target(r"C:\x\unins000.exe"));
        assert!(is_noise_target(r"C:\x\Uninstall.exe"));
        assert!(!is_noise_target(r"C:\x\chrome.exe"));
    }

    #[test]
    fn classify_lnk_outcomes() {
        // Special targets (CLSID / shell:) stay raw
        assert!(matches!(
            classify_lnk(&LnkInfo {
                target: Some(r"::{26EE0668-A00A-44D7-9371-BEB064C98683}".into()),
                ..Default::default()
            }),
            LnkOutcome::KeepRaw
        ));
        assert!(matches!(
            classify_lnk(&LnkInfo {
                target: Some(r"shell:AppsFolder".into()),
                ..Default::default()
            }),
            LnkOutcome::KeepRaw
        ));
        // Dead file target is dropped
        assert!(matches!(
            classify_lnk(&LnkInfo {
                target: Some(r"C:\does_not_exist\app.exe".into()),
                ..Default::default()
            }),
            LnkOutcome::Drop
        ));
        // Uninstaller exe is dropped
        assert!(matches!(
            classify_lnk(&LnkInfo {
                target: Some(r"C:\Program Files\X\Uninstall.exe".into()),
                ..Default::default()
            }),
            LnkOutcome::Drop
        ));
    }

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
