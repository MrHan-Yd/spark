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
    /// &mut self：实现方可在搜索路径上做顺带的维护（如历史死条目清理）。
    fn search(&mut self, query: &Query) -> Vec<Candidate>;
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

    /// 冷启动快速路径：空内存索引 + 立即加载历史（毫秒级文件读取）。
    /// Start Menu 全量扫描（数百个 .lnk 逐个 COM 解析，几百 ms）由 host 后台线程
    /// 完成后经 swap_memory_with_reconcile 换入，热键注册不再被索引构建阻塞。
    /// 默认页（空查询=最近使用历史）在索引换入前即可用。
    pub fn with_history_only() -> Self {
        let mut idx = Self::default();
        idx.history = HistoryStore::load();
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

    pub fn find_by_id(&mut self, id: &str) -> Option<Candidate> {
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

    pub fn search_with_history(&mut self, query: &Query) -> Vec<Candidate> {
        let q = query.normalized();
        if q.is_empty() {
            // 默认页只展示"打开过的"：纯最近使用（recency 序），最多 6 条一屏扫完。
            // 没有历史就空着——不拿没打开过的应用凑数，空页比无关信息好。
            return self.history.as_candidates(EMPTY_QUERY_RECENT_LIMIT);
        }

        let mut hits = self.memory.search(query);
        self.history.apply_boost(&mut hits);

        // Also surface pure history hits that match text but aren't in index.
        // 标题过滤下沉到 history（candidates_title_containing）：命中才构造
        // Candidate，且小写标题走自校验缓存——查询热路径零 to_lowercase 分配。
        for h in self.history.candidates_title_containing(&q, 50) {
            if hits.iter().any(|c| c.id == h.id) {
                continue;
            }
            hits.push(h);
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
    /// 已卸载应用的死历史条目不在这里清理——清理跟随读取路径（as_candidates 查列表
    /// 时顺带物理清理），不依赖 30s 重建定时器。
    pub fn swap_memory(&mut self, memory: MemoryIndex) {
        self.memory = memory;
    }

    /// 换入新内存索引并补做历史 reconcile（legacy .lnk id 重指向 + 孤儿清理）。
    /// 冷启动后台索引构建专用：历史在 bootstrap 已同步 load（默认页立即可用），
    /// 而 reconcile 依赖新 memory，只能在换入时补做——对齐原同步启动路径
    /// （from_start_menu 构建后立即 reconcile）的语义。30s 热更新路径沿用
    /// swap_memory（清理跟随读取路径，见其文档）。
    pub fn swap_memory_with_reconcile(&mut self, memory: MemoryIndex) {
        self.history.reconcile(&memory);
        self.memory = memory;
    }
}

impl SearchIndex for AppIndex {
    fn search(&mut self, query: &Query) -> Vec<Candidate> {
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

    fn empty_query(idx: &mut AppIndex, limit: u32) -> Vec<Candidate> {
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
        assert!(empty_query(&mut idx, 10).is_empty());
        // 有记录 → 只展示历史项，不混入没打开过的
        // （target 指向真实存在的文件：已卸载应用的死路径会被读取时清理）
        let exe = std::env::temp_dir().join(format!("spark_hist_used_{}.exe", std::process::id()));
        std::fs::write(&exe, b"").unwrap();
        let mut used = app("app.used", "Used App", 0.0);
        used.target = Some(exe.to_string_lossy().into_owned());
        idx.record_usage(&used);
        let items = empty_query(&mut idx, 10);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "app.used");
    }

    #[test]
    fn empty_query_prunes_dead_history_physically() {
        let mut idx = AppIndex::new();
        // 死目标：固定盘上已不存在的 exe（已卸载应用）
        let mut dead = app("app.dead", "Dead App", 0.5);
        dead.target = Some(r"C:\spark_no_such_dir_7f3a\quark.exe".into());
        idx.record_usage(&dead);
        // 活目标：真实存在的临时文件
        let exe = std::env::temp_dir().join(format!("spark_hist_swap_{}.exe", std::process::id()));
        std::fs::write(&exe, b"").unwrap();
        let mut alive = app("app.alive", "Alive App", 0.4);
        alive.target = Some(exe.to_string_lossy().into_owned());
        idx.record_usage(&alive);

        // 打开默认页（查最近使用列表）这一次调用顺带物理清理死条目。
        // 直接断言物理条目数——仅靠展示层断言会掩盖"读取过滤存在但清理缺失"的回归。
        let items = empty_query(&mut idx, 10);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "app.alive");
        assert_eq!(idx.history().len_for_test(), 1, "死条目随查询被物理清理");
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
        let items = empty_query(&mut idx, 10);
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
        let items = empty_query(&mut idx, 50);
        assert_eq!(
            items.len(),
            EMPTY_QUERY_RECENT_LIMIT,
            "默认页最多展示平铺 5~6 行（54 条）最近使用"
        );
        assert_eq!(items[0].id, "app.0", "最新的排最前");
        assert_eq!(items[5].id, "app.5");
    }

    #[test]
    fn search_surfaces_history_title_hits() {
        // 历史兜底路径（candidates_title_containing）：不在索引里的历史项按标题命中
        let mut idx = AppIndex::new();
        let exe = std::env::temp_dir().join(format!("spark_hist_fb_{}.exe", std::process::id()));
        std::fs::write(&exe, b"").unwrap();
        let mut used = app("app.used", "Used App", 0.0);
        used.target = Some(exe.to_string_lossy().into_owned());
        idx.record_usage(&used);

        let items = idx.search_with_history(&Query {
            text: "used".into(),
            limit: 10,
        });
        assert!(
            items.iter().any(|c| c.id == "app.used"),
            "不在索引里的历史项应按标题兜底命中"
        );
        // 不命中标题的历史项不因兜底路径出现
        let none = idx.search_with_history(&Query {
            text: "zzz_no_match".into(),
            limit: 10,
        });
        assert!(!none.iter().any(|c| c.id == "app.used"));
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
        let mut idx = AppIndex::new();
        let items = idx.search_with_history(&Query {
            text: "锁屏".into(),
            limit: 50,
        });
        assert!(
            items.iter().any(|c| c.id == "builtin.lock"),
            "无冲突时内置命令应出现"
        );
    }

    #[test]
    fn history_fallback_covers_beyond_first_50() {
        // 兜底标题过滤发生在 recency 排序之后、截断之前（candidates_title_containing）：
        // recency 排名 51+ 的历史条目标题命中时也应出现（旧实现先取 50 再过滤，永远轮不到）
        let mut idx = AppIndex::new();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        for i in 0..60 {
            idx.history_mut()
                .seed_for_test(&format!("app.{i}"), 1, now - i * 60, None);
        }
        // app.54 的 recency 排名第 55，标题 "app.54" 含查询词
        let items = idx.search_with_history(&Query {
            text: "app.54".into(),
            limit: 10,
        });
        assert!(
            items.iter().any(|c| c.id == "app.54"),
            "排名 50 之外但标题命中的历史条目应被兜底捞出"
        );
    }
}
