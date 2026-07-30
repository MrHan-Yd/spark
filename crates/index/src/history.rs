//! Local usage history for empty-query ranking.

use serde::{Deserialize, Serialize};
use spark_core::{history_path, Action, Candidate, Source};
use std::collections::HashMap;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, warn};

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

    /// Empty-query suggestions from history, newest/most-used first.
    pub fn as_candidates(&self, limit: usize) -> Vec<Candidate> {
        let mut list: Vec<&HistoryEntry> = self.entries.values().collect();
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
}
