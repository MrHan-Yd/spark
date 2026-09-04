use crate::SearchIndex;
use spark_core::{rank_candidates, Action, Candidate, Query, Source};

/// 索引条目：候选 + 预降权小写缓存。search 过滤/打分每键对全量条目做子串/
/// 子序列匹配，小写在 upsert 时算一次，避免查询热路径每键每候选 4 次
/// to_lowercase 分配（过滤 title/subtitle/target 3 次 + 打分 title 1 次）。
#[derive(Debug, Clone)]
struct IndexedApp {
    cand: Candidate,
    title_lc: String,
    sub_lc: String,
    target_lc: String,
}

impl IndexedApp {
    fn new(cand: Candidate) -> Self {
        fn lower(s: &str) -> String {
            s.to_lowercase()
        }
        let title_lc = lower(&cand.title);
        let sub_lc = cand.subtitle.as_deref().map(lower).unwrap_or_default();
        let target_lc = cand.target.as_deref().map(lower).unwrap_or_default();
        Self {
            cand,
            title_lc,
            sub_lc,
            target_lc,
        }
    }
}

/// Simple in-memory index for host bring-up and tests.
#[derive(Debug, Default, Clone)]
pub struct MemoryIndex {
    items: Vec<IndexedApp>,
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
        let mut idx = Self::new();
        for (id, title, target, score) in seed {
            let target = if target.is_empty() {
                None
            } else {
                Some(target.to_string())
            };
            idx.upsert(Candidate {
                id: id.into(),
                title: title.into(),
                subtitle: Some("应用程序".into()),
                target: target.clone(),
                icon: target,
                score,
                source: Source::App,
                actions: vec![Action::open_default()],
                plugin_id: None,
            });
        }
        idx
    }

    pub fn into_items(self) -> Vec<Candidate> {
        self.items.into_iter().map(|e| e.cand).collect()
    }

    /// 索引条目数（宿主侧后台重建时统计用；SearchIndex::len 是 trait 方法，非 pub）。
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Candidate> {
        self.items.iter().map(|e| &e.cand)
    }

    pub fn upsert(&mut self, item: Candidate) {
        let entry = IndexedApp::new(item);
        if let Some(existing) = self.items.iter_mut().find(|x| x.cand.id == entry.cand.id) {
            *existing = entry;
        } else {
            self.items.push(entry);
        }
    }
}

impl SearchIndex for MemoryIndex {
    fn search(&mut self, query: &Query) -> Vec<Candidate> {
        let q = query.normalized();
        // 单趟过滤+打分：小写全部取自条目缓存，热路径零 to_lowercase 分配。
        // 打分语义与旧实现一致：全等 +0.35 > 前缀 +0.25 > 子串 +0.12 > 子序列 +0.05。
        let mut hits: Vec<Candidate> = if q.is_empty() {
            self.items
                .iter()
                .take(query.limit as usize)
                .map(|e| e.cand.clone())
                .collect()
        } else {
            self.items
                .iter()
                .filter_map(|e| {
                    let matched = e.title_lc.contains(&q)
                        || e.sub_lc.contains(&q)
                        || e.target_lc.contains(&q)
                        || subsequence_match(&e.title_lc, &q);
                    if !matched {
                        return None;
                    }
                    let bonus = if e.title_lc == q {
                        0.35
                    } else if e.title_lc.starts_with(&q) {
                        0.25
                    } else if e.title_lc.contains(&q) {
                        0.12
                    } else {
                        0.05
                    };
                    let mut cand = e.cand.clone();
                    cand.score += bonus;
                    Some(cand)
                })
                .collect()
        };

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
        let mut idx = MemoryIndex::with_seed_apps();
        let hits = idx.search(&Query::new("term"));
        assert!(hits.iter().any(|h| h.title.contains("Terminal")));
    }

    #[test]
    fn subsequence_code() {
        assert!(subsequence_match("visual studio code", "vsc"));
        assert!(!subsequence_match("notepad", "xyz"));
    }

    #[test]
    fn upsert_replaces_keeps_lowercase_fresh() {
        let mut idx = MemoryIndex::new();
        idx.upsert(Candidate::app("app.x", "Old Name", r"C:\x.exe"));
        idx.upsert(Candidate::app("app.x", "New Name", r"C:\x.exe"));
        assert_eq!(idx.len(), 1, "同 id upsert 是替换不是追加");
        assert!(idx
            .search(&Query::new("new name"))
            .iter()
            .any(|h| h.id == "app.x"));
        assert!(
            !idx.search(&Query::new("old name"))
                .iter()
                .any(|h| h.id == "app.x"),
            "替换后旧标题缓存不得残留"
        );
    }
}
