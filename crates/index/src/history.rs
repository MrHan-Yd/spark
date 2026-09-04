//! Local usage history for empty-query ranking.

use crate::MemoryIndex;
use serde::{Deserialize, Serialize};
use spark_core::{history_path, Action, Candidate, Source};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, warn};

use crate::is_app_target;

const MAX_HISTORY: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HistoryStore {
    #[serde(default)]
    entries: HashMap<String, HistoryEntry>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
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
    /// 小写标题缓存（搜索兜底匹配用）：.0 是缓存对应的原标题，读取时比对不一致
    /// 才重算——自校验缓存，任何路径改标题都不会读到过期值；serde skip，load 后
    /// 首查自愈。用 Mutex 而非 RefCell：HistoryEntry 须满足 Send+Sync（SearchIndex
    /// trait 约束），临界区只有一次字符串比对，开销可忽略。
    /// 访问统一走 [Self::title_contains]，不直接开锁。
    #[serde(skip, default)]
    title_lc: std::sync::Mutex<(String, String)>,
}

impl Clone for HistoryEntry {
    fn clone(&self) -> Self {
        Self {
            item_id: self.item_id.clone(),
            title: self.title.clone(),
            subtitle: self.subtitle.clone(),
            target: self.target.clone(),
            icon: self.icon.clone(),
            use_count: self.use_count,
            last_used_at: self.last_used_at,
            // 缓存不随拷贝转移，克隆体首读自愈
            title_lc: std::sync::Mutex::default(),
        }
    }
}

impl HistoryEntry {
    /// 小写标题是否包含 q（读自校验缓存，miss 时重算）。
    /// 不返回锁句柄（MappedMutexGuard 在本工具链不可用），逻辑内联在临界区内。
    fn title_contains(&self, q: &str) -> bool {
        let mut cache = self
            .title_lc
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cache.0 != self.title {
            cache.1 = self.title.to_lowercase();
            cache.0.clone_from(&self.title);
        }
        cache.1.contains(q)
    }
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
                    title_lc: std::sync::Mutex::default(),
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
                title_lc: std::sync::Mutex::default(),
            },
        );
    }

    /// 测试专用：物理条目数。as_candidates 的读取过滤会掩盖物理清理的回归
    /// （死条目被滤掉后展示层看不出库里还有它），跨 crate 断言物理删除须查条目数。
    #[cfg(test)]
    pub(crate) fn len_for_test(&self) -> usize {
        self.entries.len()
    }

    /// Re-point stale history entries at the current index.
    ///
    /// Shortcut-merging changed the id space (per-shortcut id → per-exe id), so
    /// legacy entries whose target is a `.lnk` no longer match anything. They
    /// are remapped to the merged app row by unique title match, or dropped.
    /// Non-shortcut entries (files, plugins) are kept unless their id or target
    /// resolves to a live row. 已卸载应用的死条目不在这里清理——物理清理跟随
    /// 读取路径（as_candidates 查列表时顺带进行），见其文档注释。
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
    /// 清理跟随读取：应用类条目的 target 在本地固定盘上已确认消失（已卸载）时，
    /// 查列表的这一次遍历顺带物理移除并落盘（有变化才落盘）——不依赖定时器
    /// 或索引重建。盘符甄别与判死细则见 target_alive。
    pub fn as_candidates(&mut self, limit: usize) -> Vec<Candidate> {
        self.candidates_inner(limit, None)
    }

    /// 搜索兜底：与 as_candidates 相同的清理/过滤/排序，但额外按小写标题包含
    /// q 过滤（title_lc 自校验缓存，查询热路径零 to_lowercase 分配）。过滤发生在
    /// 构造 Candidate 之前——未命中的条目不再付出整串克隆的代价。
    pub fn candidates_title_containing(&mut self, q: &str, limit: usize) -> Vec<Candidate> {
        self.candidates_inner(limit, Some(q))
    }

    fn candidates_inner(&mut self, limit: usize, title_q: Option<&str>) -> Vec<Candidate> {
        let mut changed = false;
        self.entries.retain(|_, e| match e.target.as_deref() {
            Some(t) if is_app_target(t) && !target_alive(t) => {
                changed = true;
                false
            }
            _ => true,
        });
        if changed {
            #[cfg(not(test))]
            self.save();
        }

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
        if let Some(q) = title_q {
            list.retain(|e| e.title_contains(q));
        }
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

/// Win32 盘符类型：固定磁盘。
const DRIVE_FIXED: u32 = 3;

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GetDriveTypeW(lp_root_path_name: *const u16) -> u32;
}

/// 盘符根是否为本地固定磁盘。GetDriveTypeW 是本地内核查询（读重定向器的
/// 既有映射状态），不会向远端发起重连，微秒级返回，可安全放在查询热路径。
#[cfg(windows)]
fn drive_is_fixed(root: &str) -> bool {
    let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { GetDriveTypeW(wide.as_ptr()) == DRIVE_FIXED }
}

#[cfg(not(windows))]
fn drive_is_fixed(_root: &str) -> bool {
    true
}

/// 从目标中提取盘符根 `X:\`；非盘符开头形态返回 None。
/// UNC `\\...`、协议 `ms-settings:`、命令 `echo:hello`、纯 CJK 文本都不是盘符形态。
fn drive_root(target: &str) -> Option<String> {
    let bytes = target.as_bytes();
    if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' || bytes[2] != b'\\' {
        return None;
    }
    Some(format!("{}:\\", bytes[0].to_ascii_uppercase() as char))
}

/// 本地固定磁盘上的盘符路径（`X:\...`）才做文件级存在性检查；
/// 文件已删除（应用已卸载）的条目在 as_candidates 查列表时被顺带物理清理。
/// 检查前先做盘符甄别：可移动盘/映射网络盘/未知盘符一律视为存活——
/// 死亡网络盘的 stat（GetFileAttributes）会触发重连阻塞数秒，而本检查
/// 运行在 host 锁内的每次查询上，绝不能引入该阻塞。UNC/命令/协议/插件类
/// 目标同样不检查。固定盘上的 stat 是本地微秒级操作（~54-100 次/查询）。
/// 有意不做任何按文件路径的存活缓存——缓存"存在"结论会让卸载检测失效，
/// 退化回本过滤要修的原始 bug。
/// 判死语义：仅 `try_exists` 返回 Ok(false)（确认不存在）才判死；
/// stat 出错（ACL 收紧、设备错误等 Err）视为存活保留——本结果同时是
/// 物理删除（as_candidates 清理）的依据，"错误=判死"会把临时不可读
/// 升级成不可逆误删。
fn target_alive(target: &str) -> bool {
    let Some(root) = drive_root(target) else {
        return true;
    };
    if !drive_is_fixed(&root) {
        return true;
    }
    !matches!(Path::new(target).try_exists(), Ok(false))
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

    /// 真实存在的临时 exe 文件：存在性过滤只放行活文件，展示路径的用例需要真路径。
    fn temp_app_exe(tag: &str) -> String {
        let p = std::env::temp_dir().join(format!("spark_hist_{}_{}.exe", tag, std::process::id()));
        std::fs::write(&p, b"").unwrap();
        p.to_string_lossy().into_owned()
    }

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
                target: Some(temp_app_exe("once")),
                icon: None,
                use_count: 1,
                last_used_at: now - 86_400,
                ..Default::default()
            },
        );
        h.entries.insert(
            "app:freq".into(),
            HistoryEntry {
                item_id: "app:freq".into(),
                title: "Freq App".into(),
                subtitle: None,
                target: Some(temp_app_exe("freq")),
                icon: None,
                use_count: 20,
                last_used_at: now - 5 * 86_400,
                ..Default::default()
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
                ..Default::default()
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
                ..Default::default()
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
                ..Default::default()
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
                ..Default::default()
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
        let exe = temp_app_exe("docfilter");
        h.seed_for_test("app.doc", 1, now, Some(r"D:\WinRAR\WhatsNew.txt"));
        h.seed_for_test("app.exe", 1, now - 60, Some(exe.as_str()));
        h.seed_for_test("app.noext", 1, now - 120, None);
        let items = h.as_candidates(10);
        assert_eq!(items.len(), 2, "文档类历史不展示");
        assert_eq!(items[0].id, "app.exe", "应用保留且按 recency 排序");
        assert_eq!(items[1].id, "app.noext");
    }

    #[test]
    fn as_candidates_skips_dead_local_targets() {
        let mut h = HistoryStore::default();
        let now = now_secs();
        let alive = temp_app_exe("alive");
        // 已卸载应用：本地盘符路径且文件不存在 → 展示层忽略 + 物理清理
        h.seed_for_test(
            "app.dead",
            1,
            now,
            Some(r"C:\spark_no_such_dir_7f3a\quark.exe"),
        );
        // 文件仍存在的条目正常展示
        h.seed_for_test("app.alive", 1, now - 60, Some(alive.as_str()));
        // 命令/协议类目标不做存在性检查，原样保留
        h.seed_for_test("app.proto", 1, now - 120, Some("ms-settings:"));
        let items = h.as_candidates(10);
        let ids: Vec<&str> = items.iter().map(|c| c.id.as_str()).collect();
        assert!(!ids.contains(&"app.dead"), "已卸载应用的历史条目应被忽略");
        assert!(ids.contains(&"app.alive"));
        assert!(
            ids.contains(&"app.proto"),
            "非文件路径目标不受存在性检查影响"
        );
        // 查列表这一次遍历顺带物理清理：死条目从库内移除，不再占配额
        assert_eq!(h.len_for_test(), 2, "死条目被物理移除，活/命令条目保留");
    }

    #[test]
    fn drive_root_extraction() {
        assert_eq!(drive_root(r"C:\x\app.exe").as_deref(), Some(r"C:\"));
        assert_eq!(drive_root(r"c:\x").as_deref(), Some(r"C:\"));
        assert_eq!(drive_root(r"\\server\share\app.exe"), None);
        assert_eq!(drive_root("ms-settings:"), None);
        assert_eq!(drive_root("echo:hello"), None);
        assert_eq!(drive_root("夸克浏览器"), None);
        assert_eq!(drive_root("C:"), None);
        assert_eq!(drive_root(""), None);
        assert_eq!(drive_root(r"C:/x"), None, "正斜杠不是盘符形态");
        assert_eq!(drive_root(r"\\?\C:\x"), None, "扩展前缀不是盘符形态");
    }

    #[test]
    fn target_alive_skips_non_fixed_and_non_drive_forms() {
        // 本机 C:\ 是固定盘：死路径被过滤、活文件放行
        assert!(!target_alive(r"C:\spark_no_such_dir_7f3a\quark.exe"));
        assert!(target_alive(temp_app_exe("alive2").as_str()));
        // 非盘符形态（UNC/协议/命令/CJK）：直接放行，不做磁盘 IO
        assert!(target_alive("ms-settings:"));
        assert!(target_alive("echo:hello"));
        assert!(target_alive(r"\\server\share\app.exe"));
        assert!(target_alive("夸克"));
    }
}
