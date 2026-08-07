//! Search index facade. MVP: in-memory + Start Menu scan; SQLite FTS5 later.

mod apps;
mod history;
mod lnk;
mod memory;

pub use apps::enumerate_start_menu_apps;
pub use history::HistoryStore;
pub use lnk::resolve_lnk;
pub use memory::MemoryIndex;

use spark_core::{rank_candidates, Candidate, Query, Source};

pub trait SearchIndex: Send + Sync {
    fn search(&self, query: &Query) -> Vec<Candidate>;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// App index + history fused search (P0).
#[derive(Debug, Default)]
pub struct AppIndex {
    memory: MemoryIndex,
    history: HistoryStore,
}

impl AppIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build from Start Menu enumeration (may take a few hundred ms).
    pub fn from_start_menu() -> Self {
        let mut memory = MemoryIndex::new();
        for app in enumerate_start_menu_apps() {
            memory.upsert(app);
        }
        let mut history = HistoryStore::load();
        // Shortcut merging changed the id space; re-point stale history rows
        history.reconcile(&memory);
        Self { memory, history }
    }

    pub fn with_seed_fallback() -> Self {
        let mut idx = Self::from_start_menu();
        if idx.memory.len() < 3 {
            // Dev / locked-down environments
            for app in MemoryIndex::with_seed_apps().into_items() {
                idx.memory.upsert(app);
            }
        }
        idx
    }

    pub fn history_mut(&mut self) -> &mut HistoryStore {
        &mut self.history
    }

    pub fn history(&self) -> &HistoryStore {
        &self.history
    }

    pub fn record_usage(&mut self, item: &Candidate) {
        self.history.record(item);
    }

    pub fn find_by_id(&self, id: &str) -> Option<Candidate> {
        self.memory
            .iter()
            .find(|c| c.id == id)
            .cloned()
            .or_else(|| {
                self.history
                    .as_candidates(100)
                    .into_iter()
                    .find(|c| c.id == id)
            })
    }

    pub fn search_with_history(&self, query: &Query) -> Vec<Candidate> {
        let q = query.normalized();
        if q.is_empty() {
            let mut items = self.history.as_candidates(query.limit as usize);
            if items.len() < query.limit as usize {
                // Pad with top apps not already in history
                let have: std::collections::HashSet<_> =
                    items.iter().map(|c| c.id.clone()).collect();
                for c in self.memory.iter() {
                    if items.len() >= query.limit as usize {
                        break;
                    }
                    if !have.contains(&c.id) {
                        let mut clone = c.clone();
                        clone.score *= 0.9;
                        items.push(clone);
                    }
                }
            }
            return rank_candidates(items);
        }

        let mut hits = self.memory.search(query);
        self.history.apply_boost(&mut hits);

        // Also surface pure history hits that match text but aren't in index
        for h in self.history.as_candidates(50) {
            if hits.iter().any(|c| c.id == h.id) {
                continue;
            }
            let title = h.title.to_lowercase();
            if title.contains(&q) {
                hits.push(h);
            }
        }

        hits = rank_candidates(hits);
        hits.truncate(query.limit as usize);
        hits
    }

    pub fn len(&self) -> usize {
        self.memory.len()
    }

    pub fn rebuild_apps(&mut self) {
        let mut memory = MemoryIndex::new();
        for app in enumerate_start_menu_apps() {
            memory.upsert(app);
        }
        self.memory = memory;
    }
}

impl SearchIndex for AppIndex {
    fn search(&self, query: &Query) -> Vec<Candidate> {
        self.search_with_history(query)
    }

    fn len(&self) -> usize {
        self.memory.len()
    }
}

/// Helper re-export style source tag check
pub fn is_launchable(c: &Candidate) -> bool {
    matches!(
        c.source,
        Source::App | Source::History | Source::Favorite | Source::File | Source::Builtin
    ) && c.target.as_ref().map(|t| !t.is_empty()).unwrap_or(false)
}
