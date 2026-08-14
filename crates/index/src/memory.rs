use crate::SearchIndex;
use spark_core::{rank_candidates, Action, Candidate, Query, Source};

/// Simple in-memory index for host bring-up and tests.
#[derive(Debug, Default, Clone)]
pub struct MemoryIndex {
    items: Vec<Candidate>,
}

impl MemoryIndex {
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    pub fn with_seed_apps() -> Self {
        let seed = [
            ("app.wt", "Windows Terminal", r"wt.exe", 1.0_f32),
            ("app.code", "Visual Studio Code", r"code.cmd", 0.95),
            ("app.chrome", "Google Chrome", r"chrome.exe", 0.9),
            (
                "app.explorer",
                "文件资源管理器",
                r"C:\Windows\explorer.exe",
                0.85,
            ),
            ("sys.settings", "Spark 设置", "", 0.8),
        ];
        let items = seed
            .into_iter()
            .map(|(id, title, target, score)| {
                let target = if target.is_empty() {
                    None
                } else {
                    Some(target.to_string())
                };
                Candidate {
                    id: id.into(),
                    title: title.into(),
                    subtitle: Some("应用程序".into()),
                    target: target.clone(),
                    icon: target,
                    score,
                    source: Source::App,
                    actions: vec![Action::open_default()],
                    plugin_id: None,
                }
            })
            .collect();
        Self { items }
    }

    pub fn into_items(self) -> Vec<Candidate> {
        self.items
    }

    /// 索引条目数（宿主侧后台重建时统计用；SearchIndex::len 是 trait 方法，非 pub）。
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Candidate> {
        self.items.iter()
    }

    pub fn upsert(&mut self, item: Candidate) {
        if let Some(existing) = self.items.iter_mut().find(|x| x.id == item.id) {
            *existing = item;
        } else {
            self.items.push(item);
        }
    }
}

impl SearchIndex for MemoryIndex {
    fn search(&self, query: &Query) -> Vec<Candidate> {
        let q = query.normalized();
        let mut hits: Vec<Candidate> = if q.is_empty() {
            self.items
                .iter()
                .take(query.limit as usize)
                .cloned()
                .collect()
        } else {
            self.items
                .iter()
                .filter(|c| {
                    let title = c.title.to_lowercase();
                    let sub = c
                        .subtitle
                        .as_ref()
                        .map(|s| s.to_lowercase())
                        .unwrap_or_default();
                    let target = c
                        .target
                        .as_ref()
                        .map(|s| s.to_lowercase())
                        .unwrap_or_default();
                    // Subsequence-ish: all chars of q appear in order in title, or plain contains
                    title.contains(&q)
                        || sub.contains(&q)
                        || target.contains(&q)
                        || subsequence_match(&title, &q)
                })
                .cloned()
                .collect()
        };

        for c in &mut hits {
            let title = c.title.to_lowercase();
            if !q.is_empty() && title == q {
                c.score += 0.35;
            } else if title.starts_with(&q) {
                c.score += 0.25;
            } else if title.contains(&q) {
                c.score += 0.12;
            } else if subsequence_match(&title, &q) {
                c.score += 0.05;
            }
        }

        hits = rank_candidates(hits);
        hits.truncate(query.limit as usize);
        hits
    }

    fn len(&self) -> usize {
        self.items.len()
    }
}

/// True if all chars of `pat` appear in order inside `text` (fuzzy light).
fn subsequence_match(text: &str, pat: &str) -> bool {
    if pat.is_empty() {
        return true;
    }
    let mut it = text.chars();
    for pc in pat.chars() {
        loop {
            match it.next() {
                Some(tc) if tc == pc => break,
                Some(_) => continue,
                None => return false,
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_terminal() {
        let idx = MemoryIndex::with_seed_apps();
        let hits = idx.search(&Query::new("term"));
        assert!(hits.iter().any(|h| h.title.contains("Terminal")));
    }

    #[test]
    fn subsequence_code() {
        assert!(subsequence_match("visual studio code", "vsc"));
        assert!(!subsequence_match("notepad", "xyz"));
    }
}
