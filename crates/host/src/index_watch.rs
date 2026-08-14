//! Start Menu change detection + background index refresh.
//!
//! 新安装/卸载应用时热更新索引，无需重启 host（否则新装应用搜不到）。
//! win_loop 每 30s 投递一次 WM_TIMER → `poll`：先算目录指纹（毫秒级，
//! 只 stat 目录不解析 .lnk），变化了才在后台线程重建索引并原子换入，
//! 重建期间不占用 host 锁，查询/热键不受影响。

use crate::app::SharedHost;
use spark_index::{enumerate_start_menu_apps, start_menu_fingerprint, MemoryIndex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tracing::{info, warn};

/// 上次全量重建时的目录指纹（0 = 未建立基线）。
static LAST_FP: Mutex<u64> = Mutex::new(0);
/// 重建进行中标记，防止定时器与重建线程交错触发第二次重建。
static REFRESHING: AtomicBool = AtomicBool::new(false);

/// 启动时记录基线：把当前指纹当作"索引刚建好"的状态，
/// 之后任何与它不一致的变化都会触发重建。
pub fn record_baseline() {
    let fp = start_menu_fingerprint();
    if let Ok(mut g) = LAST_FP.lock() {
        *g = fp;
    }
}

/// 检查 Start Menu 是否变化；变了就在后台重建索引并原子换入。
pub fn poll(host: &SharedHost) {
    let fp = start_menu_fingerprint();
    let last = LAST_FP.lock().map(|g| *g).unwrap_or(0);
    if fp == last || last == 0 {
        return;
    }
    if REFRESHING.swap(true, Ordering::SeqCst) {
        return;
    }
    info!("start menu changed; rebuilding index");
    let host2 = host.clone();
    std::thread::spawn(move || {
        let mut mem = MemoryIndex::new();
        for app in enumerate_start_menu_apps() {
            mem.upsert(app);
        }
        let n = mem.len();
        match host2.lock() {
            Ok(mut g) => {
                g.index.swap_memory(mem);
                info!(apps = n, "app index refreshed");
            }
            Err(e) => warn!(?e, "host lock poisoned; index refresh skipped"),
        }
        if let Ok(mut f) = LAST_FP.lock() {
            *f = fp;
        }
        REFRESHING.store(false, Ordering::SeqCst);
    });
}
