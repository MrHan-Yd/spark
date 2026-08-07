//! Local usage history for empty-query ranking.

use crate::MemoryIndex;
use serde::{Deserialize, Serialize};
use spark_core::{history_path, Action, Candidate, Source};
use std::collections::HashMap;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, warn};

use crate::is_app_target;

const MAX_HISTORY: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HistoryStore {
    #[serde(default)]
    entries: HashMap<String, HistoryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub item_id: String,
    pub title: String,
    #[serde(default)]
    pub subtitle: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    pub use_count: u32,
    pub last_used_at: u64,
}

impl HistoryStore {
    pub fn load() -> Self {
        let path = history_path();
        match fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(s) => s,
                Err(e) => {
                    warn!(?e, path = %path.display(), "history corrupt; starting fresh");
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let path = history_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(self) {
            Ok(text) => {
                if let Err(e) = fs::write(&path, text) {
                    warn!(?e, "failed to write history");
                }
            }
            Err(e) => warn!(?e, "history serialize failed"),
        }
    }

    pub fn record(&mut self, item: &Candidate) {
        let now = now_secs();
        let count = {
            let entry = self
                .entries
                .entry(item.id.clone())
                .or_insert_with(|| HistoryEntry {
                    item_id: item.id.clone(),
                    title: item.title.clone(),
                    subtitle: item.subtitle.clone(),
                    target: item.target.clone(),
                    icon: item.icon.clone(),
                    use_count: 0,
                    last_used_at: now,
                });
            entry.title = item.title.clone();
            entry.subtitle = item.subtitle.clone();
            entry.target = item.target.clone();
            entry.icon = item.icon.clone();
            entry.use_count = entry.use_count.saturating_add(1);
            entry.last_used_at = now;
            entry.use_count
        };
        self.trim();
        // Avoid writing into real %APPDATA% during unit tests
        #[cfg(not(test))]
        self.save();
        debug!(id = %item.id, count, "history recorded");
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.save();
    }

    /// 测试专用：直接写入一条历史（绕过 record 的 now 时间戳，便于构造时间顺序），
    /// 供 crate 内集成测试（lib.rs tests）使用。
    #[cfg(test)]
    pub(crate) fn seed_for_test(
        &mut self,
        id: &str,
        use_count: u32,
        last_used_at: u64,
        target: Option<&str>,
    ) {
        self.entries.insert(
            id.to_string(),
            HistoryEntry {
                item_id: id.to_string(),
                title: id.to_string(),
                subtitle: None,
                target: target.map(|t| t.to_string()),
                icon: None,
                use_count,
                last_used_at,
            },
        );
    }

    /// Re-point stale history entries at the current index.
    ///
    /// Shortcut-merging changed the id space (per-shortcut id → per-exe id), so
    /// legacy entries whose target is a `.lnk` no longer match anything. They
    /// are remapped to the merged app row by unique title match, or dropped.
    /// Non-shortcut entries (files, plugins) are kept unless their id or target
    /// resolves to a live row.
    pub fn reconcile(&mut self, index: &MemoryIndex) {
        let mut by_target: HashMap<String, &Candidate> = HashMap::new();
        let mut by_title: HashMap<String, Vec<&Candidate>> = HashMap::new();
        for c in index.iter() {
            if let Some(t) = &c.target {
                by_target.insert(norm_path(t), c);
            }
            by_title.entry(c.title.to_lowercase()).or_default().push(c);
        }
        let live_ids: std::collections::HashSet<&str> =
            index.iter().map(|c| c.id.as_str()).collect();

        let mut next: HashMap<String, HistoryEntry> = HashMap::new();
        for (key, mut e) in std::mem::take(&mut self.entries) {
            if live_ids.contains(e.item_id.as_str()) {
                next.insert(key, e);
                continue;
            }
            let is_lnk_legacy = e
                .target
                .as_deref()
                .map(|t| t.to_lowercase().ends_with(".lnk"))
                .unwrap_or(false);
            let replacement = e
                .target
                .as_deref()
                .and_then(|t| by_target.get(&norm_path(t)).copied())
                .or_else(|| {
                    if !is_lnk_legacy {
                        return None;
                    }
                    by_title
                        .get(&e.title.to_lowercase())
                        .filter(|v| v.len() == 1)
                        .map(|v| v[0])
                });
            match replacement {
                Some(c) => {
                    e.item_id = c.id.clone();
                    e.title = c.title.clone();
                    e.subtitle = c.subtitle.clone();
                    e.target = c.target.clone();
                    e.icon = c.icon.clone();
                    next.insert(c.id.clone(), e);
                }
                // Stale shortcut history with no live row → drop
                None if is_lnk_legacy => {}
                // 文档类文件历史（txt/word 等）：不匹配应用索引，也不在 as_candidates
                // 里展示——直接清理，避免 history.json 留死数据
                None if e
                    .target
                    .as_deref()
                    .map(|t| !is_app_target(t))
                    .unwrap_or(false) => {}
                // 其余非应用索引条目（命令/插件等）保留
                None => {
                    next.insert(key, e);
                }
            }
        }
        self.entries = next;
        // Runs once at host startup; unconditional save is fine
        #[cfg(not(test))]
        self.save();
    }

    /// Empty-query suggestions from history: 最近使用优先（last_used_at 降序，
    /// use_count 仅作同秒平局判断）。不做频率加权——默认页语义是"最近使用"，
    /// 不是"最常用"；频率信号只用在搜索路径的 apply_boost 里。
    /// 只输出应用：文档类文件历史（txt/word 等）不展示、不参与搜索。
    pub fn as_candidates(&self, limit: usize) -> Vec<Candidate> {
        let mut list: Vec<&HistoryEntry> = self
            .entries
            .values()
            .filter(|e| e.target.as_deref().map(is_app_target).unwrap_or(true))
            .collect();
        list.sort_by(|a, b| {
            b.last_used_at
                .cmp(&a.last_used_at)
                .then(b.use_count.cmp(&a.use_count))
        });
        list.into_iter()
            .take(limit)
            .map(|e| {
                let recency = recency_score(e.last_used_at);
                let freq = (1.0 + (e.use_count as f32).ln()).min(3.0) * 0.15;
                Candidate {
                    id: e.item_id.clone(),
                    title: e.title.clone(),
                    subtitle: e.subtitle.clone().or_else(|| Some("最近使用".into())),
                    target: e.target.clone(),
                    icon: e.icon.clone(),
                    score: 0.7 + recency + freq,
                    source: Source::History,
                    actions: vec![Action::open_default()],
                    plugin_id: None,
                }
            })
            .collect()
    }

    /// Boost score of matching live candidates by usage.
    pub fn apply_boost(&self, items: &mut [Candidate]) {
        let now = now_secs();
        for c in items.iter_mut() {
            if let Some(e) = self.entries.get(&c.id) {
                let recency = recency_score_at(e.last_used_at, now);
                let freq = (1.0 + (e.use_count as f32).ln()).min(3.0) * 0.12;
                c.score += recency + freq;
            }
        }
    }

    fn trim(&mut self) {
        if self.entries.len() <= MAX_HISTORY {
            return;
        }
        let mut list: Vec<(String, u64, u32)> = self
            .entries
            .iter()
            .map(|(k, v)| (k.clone(), v.last_used_at, v.use_count))
            .collect();
        list.sort_by(|a, b| a.1.cmp(&b.1).then(a.2.cmp(&b.2)));
        let drop_n = self.entries.len() - MAX_HISTORY;
        for (k, _, _) in list.into_iter().take(drop_n) {
            self.entries.remove(&k);
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Lowercase + strip `\\?\` extended-path prefixes for target comparison.
fn norm_path(s: &str) -> String {
    let t = if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s.to_string()
    };
    t.to_lowercase()
}

fn recency_score(last: u64) -> f32 {
    recency_score_at(last, now_secs())
}

fn recency_score_at(last: u64, now: u64) -> f32 {
    let age_hours = now.saturating_sub(last) as f32 / 3600.0;
    // ~0.35 within an hour, decays toward 0 over ~2 weeks
    (0.35 * (-age_hours / (24.0 * 7.0)).exp()).clamp(0.0, 0.4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use spark_core::Candidate;

    #[test]
    fn record_and_boost() {
        let mut h = HistoryStore::default();
        let c = Candidate::app("t1", "Test App", "C:\\test.exe");
        h.record(&c);
        assert_eq!(h.entries.get("t1").map(|e| e.use_count), Some(1));
        let mut items = vec![c];
        h.apply_boost(&mut items);
        assert!(items[0].score > 1.0);
    }

    #[test]
    fn as_candidates_orders_by_recency() {
        let mut h = HistoryStore::default();
        let now = now_secs();
        // 昨天碰巧开过一次（更近）必须排在"高频但 5 天前"前面——默认页是最近使用，
        // 频率不参与排序
        h.entries.insert(
            "app:once".into(),
            HistoryEntry {
                item_id: "app:once".into(),
                title: "Once App".into(),
                subtitle: None,
                target: Some(r"C:\once.exe".into()),
                icon: None,
                use_count: 1,
                last_used_at: now - 86_400,
            },
        );
        h.entries.insert(
            "app:freq".into(),
            HistoryEntry {
                item_id: "app:freq".into(),
                title: "Freq App".into(),
                subtitle: None,
                target: Some(r"C:\freq.exe".into()),
                icon: None,
                use_count: 20,
                last_used_at: now - 5 * 86_400,
            },
        );
        let items = h.as_candidates(10);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "app:once", "最近使用的排最前，不管用了多少次");
        assert_eq!(items[1].id, "app:freq");
    }

    #[test]
    fn reconcile_repairs_stale_shortcut_entries() {
        let mut h = HistoryStore::default();
        h.entries.insert(
            "app:old".into(),
            HistoryEntry {
                item_id: "app:old".into(),
                title: "Google Chrome".into(),
                subtitle: Some("开始菜单".into()),
                target: Some(r"C:\sm\Google Chrome.lnk".into()),
                icon: Some(r"C:\sm\Google Chrome.lnk".into()),
                use_count: 3,
                last_used_at: 1000,
            },
        );
        let mut idx = MemoryIndex::new();
        idx.upsert(Candidate::app(
            "app:new",
            "Google Chrome",
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        ));
        h.reconcile(&idx);
        assert_eq!(h.entries.len(), 1);
        let e = h.entries.get("app:new").expect("remapped to new id");
        assert_eq!(
            e.target.as_deref(),
            Some(r"C:\Program Files\Google\Chrome\Application\chrome.exe")
        );
        assert_eq!(e.use_count, 3);
    }

    #[test]
    fn reconcile_drops_orphan_shortcut() {
        let mut h = HistoryStore::default();
        h.entries.insert(
            "app:gone".into(),
            HistoryEntry {
                item_id: "app:gone".into(),
                title: "Ghost App".into(),
                subtitle: None,
                target: Some(r"C:\sm\Ghost App.lnk".into()),
                icon: None,
                use_count: 1,
                last_used_at: 1000,
            },
        );
        let idx = MemoryIndex::new();
        h.reconcile(&idx);
        assert!(h.entries.is_empty());
    }

    #[test]
    fn reconcile_keeps_command_entries() {
        let mut h = HistoryStore::default();
        // 命令/协议类目标（非文档文件）不属于应用索引，但保留（插件等）
        h.entries.insert(
            "cmd:echo".into(),
            HistoryEntry {
                item_id: "cmd:echo".into(),
                title: "Echo".into(),
                subtitle: Some("插件".into()),
                target: Some("echo:hello".into()),
                icon: None,
                use_count: 2,
                last_used_at: 1000,
            },
        );
        let idx = MemoryIndex::new();
        h.reconcile(&idx);
        assert_eq!(h.entries.len(), 1, "命令类历史保留");
    }

    #[test]
    fn reconcile_drops_document_file_entries() {
        let mut h = HistoryStore::default();
        h.entries.insert(
            "app:doc".into(),
            HistoryEntry {
                item_id: "app:doc".into(),
                title: "说明文档".into(),
                subtitle: None,
                target: Some(r"D:\x\readme.txt".into()),
                icon: None,
                use_count: 1,
                last_used_at: 1000,
            },
        );
        let idx = MemoryIndex::new();
        h.reconcile(&idx);
        assert!(h.entries.is_empty(), "文档类历史在 reconcile 时清理");
    }

    #[test]
    fn as_candidates_filters_document_files() {
        let mut h = HistoryStore::default();
        let now = now_secs();
        h.seed_for_test("app.doc", 1, now, Some(r"D:\WinRAR\WhatsNew.txt"));
        h.seed_for_test("app.exe", 1, now - 60, Some(r"D:\WinRAR\WinRAR.exe"));
        h.seed_for_test("app.noext", 1, now - 120, None);
        let items = h.as_candidates(10);
        assert_eq!(items.len(), 2, "文档类历史不展示");
        assert_eq!(items[0].id, "app.exe", "应用保留且按 recency 排序");
        assert_eq!(items[1].id, "app.noext");
    }
}
