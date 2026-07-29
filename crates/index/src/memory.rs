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
            ("app.wt", "Windows Terminal", 1.0_f32),
            ("app.code", "Visual Studio Code", 0.95),
            ("app.chrome", "Google Chrome", 0.9),
            ("app.explorer", "文件资源管理器", 0.85),
            ("sys.settings", "Spark 设置", 0.8),
        ];
        let items = seed
            .into_iter()
            .map(|(id, title, score)| Candidate {
                id: id.into(),
                title: title.into(),
                subtitle: Some("应用程序".into()),
                score,
                source: Source::App,
                actions: vec![Action::open_default()],
                plugin_id: None,
            })
            .collect();
        Self { items }
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
            self.items.iter().take(query.limit as usize).cloned().collect()
        } else {
            self.items
                .iter()
                .filter(|c| {
                    c.title.to_lowercase().contains(&q)
                        || c.subtitle
                            .as_ref()
                            .map(|s| s.to_lowercase().contains(&q))
                            .unwrap_or(false)
                })
                .cloned()
                .collect()
        };

        for c in &mut hits {
            let title = c.title.to_lowercase();
            if title.starts_with(&q) {
                c.score += 0.2;
            } else if title.contains(&q) {
                c.score += 0.1;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_terminal() {
        let idx = MemoryIndex::with_seed_apps();
        let hits = idx.search(&Query::new("term"));
        assert!(hits.iter().any(|h| h.title.contains("Terminal")));
    }
}
