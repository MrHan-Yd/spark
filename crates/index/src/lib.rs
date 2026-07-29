//! Search index facade. MVP: in-memory; production: SQLite FTS5.

mod memory;

pub use memory::MemoryIndex;

use spark_core::{Candidate, Query};

pub trait SearchIndex: Send + Sync {
    fn search(&self, query: &Query) -> Vec<Candidate>;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
