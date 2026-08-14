//! Search index facade. MVP: in-memory + Start Menu scan; SQLite FTS5 later.

mod apps;
pub mod builtin;
mod history;
mod lnk;
mod memory;

pub use apps::{enumerate_start_menu_apps, start_menu_fingerprint};
pub use history::HistoryStore;
pub use lnk::resolve_lnk;
pub use memory::MemoryIndex;

use spark_core::{rank_candidates, Candidate, Query, Source};
use std::path::Path;

/// 目标是否为"应用"：文档类文件（txt/word/pdf 等）不是应用，默认页与搜索都不展示。
/// 无扩展名目标（命令/协议/插件）一律视为应用保留。按扩展名判断——不做磁盘 IO，
/// 历史里指向已删除文件的条目也能正确过滤。
pub(crate) fn is_app_target(target: &str) -> bool {
    let ext = Path::new(target)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    matches!(
        ext.as_str(),
        "" | "exe" | "com" | "bat" | "cmd" | "msc" | "cpl" | "scr" | "lnk"
    )
}

/// 两个标题是否视为同一功能（相同或一方包含另一方，忽略大小写）。
/// 用于内置命令与开始菜单应用去重，如"文件资源管理" vs "文件资源管理器"。
fn titles_overlap(a: &str, b: &str) -> bool {
    let (a, b) = (a.to_lowercase(), b.to_lowercase());
    a == b || a.contains(&b) || b.contains(&a)
}

pub trait SearchIndex: Send + Sync {
    fn search(&self, query: &Query) -> Vec<Candidate>;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 默认页（空查询）最多展示的最近使用条数：平铺视图 5~6 行 × 9 列 ≈ 54 个。
/// 按窗口高度 590px 算，结果区实际可完整显示约 5 行，多出的行由 GridView 滚动承接。
const EMPTY_QUERY_RECENT_LIMIT: usize = 54;

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
            // 默认页只展示"打开过的"：纯最近使用（recency 序），最多 6 条一屏扫完。
            // 没有历史就空着——不拿没打开过的应用凑数，空页比无关信息好。
            return self.history.as_candidates(EMPTY_QUERY_RECENT_LIMIT);
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

        // 内置系统命令（utools 风格）：与应用混排，靠分数与别名区分。
        // 与已命中的应用/历史候选标题重复（相同/互相包含）时跳过——开始菜单里已有
        // 同功能应用（如"文件资源管理器"）就不重复展示内置命令，内置命令兜底。
        // 内置命令之间不去重（"回收站"与"清空回收站"是不同命令，标题包含只是巧合）。
        for b in builtin::candidates(&q) {
            let duplicated = hits
                .iter()
                .any(|c| c.source != Source::Builtin && titles_overlap(&c.title, &b.title));
            if !duplicated {
                hits.push(b);
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

    /// 后台重建完成后原子换入新内存索引（历史记录保留），避免重建期间长时间占用锁。
    pub fn swap_memory(&mut self, memory: MemoryIndex) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use spark_core::{Action, Candidate, Query, Source};

    fn app(id: &str, title: &str, score: f32) -> Candidate {
        Candidate {
            id: id.into(),
            title: title.into(),
            subtitle: Some("应用程序".into()),
            target: Some("C:\\apps\\x.exe".into()),
            icon: None,
            score,
            source: Source::App,
            actions: vec![Action::open_default()],
            plugin_id: None,
        }
    }

    fn empty_query(idx: &AppIndex, limit: u32) -> Vec<Candidate> {
        idx.search_with_history(&Query {
            text: String::new(),
            limit,
        })
    }

    #[test]
    fn empty_query_shows_only_history() {
        let mut idx = AppIndex::new();
        idx.memory.upsert(app("app.pad", "Pad App", 0.95));
        // 没有打开记录 → 空查询不展示任何兜底应用
        assert!(empty_query(&idx, 10).is_empty());
        // 有记录 → 只展示历史项，不混入没打开过的
        idx.record_usage(&app("app.used", "Used App", 0.0));
        let items = empty_query(&idx, 10);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "app.used");
    }

    #[test]
    fn empty_query_history_orders_by_recency() {
        let mut idx = AppIndex::new();
        idx.memory.upsert(app("app.pad", "Pad App", 0.95));
        // 直接构造历史（record 会用同一时刻，无法区分先后）
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        idx.history_mut()
            .seed_for_test("app.old", 20, now - 86_400, None);
        idx.history_mut()
            .seed_for_test("app.new", 1, now - 60, None);
        let items = empty_query(&idx, 10);
        assert_eq!(items.len(), 2);
        // 纯 recency 序：刚用过的在最前（哪怕 app.old 用了 20 次），不被分数重排
        assert_eq!(items[0].id, "app.new");
        assert_eq!(items[1].id, "app.old");
    }

    #[test]
    fn empty_query_recents_capped() {
        let mut idx = AppIndex::new();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        for i in 0..60 {
            idx.history_mut()
                .seed_for_test(&format!("app.{i}"), 1, now - i * 60, None);
        }
        let items = empty_query(&idx, 50);
        assert_eq!(
            items.len(),
            EMPTY_QUERY_RECENT_LIMIT,
            "默认页最多展示平铺 5~6 行（54 条）最近使用"
        );
        assert_eq!(items[0].id, "app.0", "最新的排最前");
        assert_eq!(items[5].id, "app.5");
    }

    #[test]
    fn builtin_deduped_against_same_app() {
        // 开始菜单已有"文件资源管理器"（explorer.exe）时，内置"文件资源管理"不重复展示
        let mut idx = AppIndex::new();
        idx.memory
            .upsert(app("sys.explorer", "文件资源管理器", 0.85));
        let items = idx.search_with_history(&Query {
            text: "文件资源".into(),
            limit: 50,
        });
        let explorer: Vec<_> = items
            .iter()
            .filter(|c| c.title.contains("文件资源"))
            .collect();
        assert_eq!(explorer.len(), 1, "同功能只保留一个");
        assert_eq!(explorer[0].source, Source::App, "保留开始菜单应用版本");
    }

    #[test]
    fn builtin_shows_when_no_app_conflict() {
        // 开始菜单没有同功能应用时，内置命令正常出现
        let idx = AppIndex::new();
        let items = idx.search_with_history(&Query {
            text: "锁屏".into(),
            limit: 50,
        });
        assert!(
            items.iter().any(|c| c.id == "builtin.lock"),
            "无冲突时内置命令应出现"
        );
    }
}
