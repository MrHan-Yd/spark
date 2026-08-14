//! Hidden message-only window: hotkey + tray pump.

use crate::app::SharedHost;
use crate::hotkey::{Hotkey, HOTKEY_ID_TOGGLE, WM_SPARK_TRAY};
use crate::ipc_server::UiHub;
use crate::tray::{self, TrayIcon, CMD_EXIT, CMD_SHOW, CMD_TOGGLE_HOTKEY};
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::{error, info, warn};
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, KillTimer, PostQuitMessage,
    RegisterClassW, SetTimer, TranslateMessage, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, MSG,
    WINDOW_EX_STYLE, WM_DESTROY, WM_HOTKEY, WM_TIMER, WM_USER, WNDCLASSW, WS_OVERLAPPED,
};

static HOTKEY_PAUSED: AtomicBool = AtomicBool::new(false);
/// 主消息循环线程持有的 HostApp / UiHub Arc（消息处理与后台线程共用）。
/// 注意：不能只存 `Arc::as_ptr` 的裸指针再 `(*ptr).clone()` 还原 —— as_ptr 返回的是
/// ArcInner 内 T 的地址（即 Mutex<HostApp> 本体），按 Arc 布局去读会把 Mutex 的
/// SRWLOCK 内部值当成 data 指针，未锁定时为 0，`lock inc [0]` 直接访问违例
/// （v0.2.7 WM_TIMER 定时器实测崩溃 0xc0000005 @0x56d40）。必须持有 Arc 本体再克隆。
static mut HOST_ARC: Option<SharedHost> = None;
static mut HUB_ARC: Option<Arc<UiHub>> = None;
/// 主消息循环窗口句柄（ipc_server 收到 host.exit 时投递退出消息用）。
pub(crate) static mut EXIT_HWND: Option<HWND> = None;
/// IPC host.exit → 主窗口自定义消息，走与托盘"退出"相同的退出路径。
pub(crate) const WM_SPARK_EXIT: u32 = WM_USER + 2;
/// IPC host.set_config（热键变更）→ 主窗口重注册全局热键。
pub(crate) const WM_SPARK_REHOTKEY: u32 = WM_USER + 3;
/// Start Menu 变更检测定时器（每 30s 一次指纹，变化了才重建索引）。
const INDEX_TIMER_ID: usize = 0x5350; // "SP"

pub fn run(host: SharedHost, hub: UiHub, icon_path: Option<PathBuf>) -> Result<()> {
    let class_name = w!("SparkHostMsgWindow");

    let instance = unsafe { GetModuleHandleW(None)? };
    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wnd_proc),
        hInstance: instance.into(),
        lpszClassName: class_name,
        ..Default::default()
    };
    let atom = unsafe { RegisterClassW(&wc) };
    if atom == 0 {
        anyhow::bail!("RegisterClassW failed");
    }

    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            w!("Spark Host"),
            WS_OVERLAPPED,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            None,
            None,
            Some(instance.into()),
            None,
        )?
    };

    let hub = Arc::new(hub);
    unsafe {
        HOST_ARC = Some(host.clone());
        HUB_ARC = Some(hub.clone());
        EXIT_HWND = Some(hwnd);
    }

    {
        let cfg = host.lock().unwrap().config.clone();
        HOTKEY_PAUSED.store(!cfg.hotkey_enabled, Ordering::SeqCst);
        if cfg.hotkey_enabled {
            match Hotkey::parse(&cfg.hotkey_toggle) {
                Ok(hk) => match hk.register(hwnd, HOTKEY_ID_TOGGLE) {
                    Ok(()) => info!(hotkey = %cfg.hotkey_toggle, "hotkey registered"),
                    Err(e) => error!(?e, "hotkey register failed"),
                },
                Err(e) => error!(?e, raw = %cfg.hotkey_toggle, "invalid hotkey"),
            }
        } else {
            info!("hotkey disabled by config");
        }
    }

    let tray = TrayIcon::add(hwnd, "Spark", icon_path.as_deref()).context("tray icon")?;
    // 托盘由 host 持有（invoke 气泡提示要用）；消息循环结束时随 host drop 自动 NIM_DELETE
    host.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?.tray = Some(tray);
    info!(
        ui_clients = hub.client_count(),
        "tray ready; message loop running"
    );

    // 索引热更新：记录基线后每 30s 检测 Start Menu 变化（新装应用无需重启即可搜到）
    crate::index_watch::record_baseline();
    unsafe {
        SetTimer(Some(hwnd), INDEX_TIMER_ID, 30_000, None);
    }

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    unsafe {
        let _ = KillTimer(Some(hwnd), INDEX_TIMER_ID);
    };
    Hotkey::unregister(hwnd, HOTKEY_ID_TOGGLE);
    unsafe {
        HOST_ARC = None;
        HUB_ARC = None;
        EXIT_HWND = None;
    }
    // keep hub alive until end
    drop(hub);
    Ok(())
}

/// 退出整个应用：广播 ui.exit 让 UI（独立进程）一起退出，再结束本进程消息循环。
/// 托盘"退出"与 IPC host.exit 共用此路径。
/// 退出整个应用：信号 EXIT_EVENT 让 UI（独立进程）一起退出，再结束本进程消息循环。
/// 托盘"退出"与 IPC host.exit 共用此路径。
/// 注：不用 pipe 广播通知 UI——host 管道句柄是同步的，read_loop 的 ReadFile 挂起时
/// 同句柄 WriteFile 会阻塞到读方向返回（实测 11-30s 延迟），命名事件无此问题。
fn exit_app() {
    info!("app exit requested");
    crate::toggle_signal::signal_exit();
    unsafe {
        PostQuitMessage(0);
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_HOTKEY => {
            if wparam.0 as i32 == HOTKEY_ID_TOGGLE && !HOTKEY_PAUSED.load(Ordering::SeqCst) {
                on_toggle();
            }
            LRESULT(0)
        }
        m if m == WM_SPARK_TRAY => {
            let paused = HOTKEY_PAUSED.load(Ordering::SeqCst);
            if let Some(cmd) = tray::handle_tray_message(hwnd, lparam, paused) {
                match cmd {
                    CMD_SHOW => on_toggle(),
                    CMD_TOGGLE_HOTKEY => toggle_hotkey_pause(hwnd),
                    CMD_EXIT => {
                        info!("exit from tray");
                        exit_app();
                    }
                    _ => {}
                }
            }
            LRESULT(0)
        }
        m if m == WM_SPARK_EXIT => {
            exit_app();
            LRESULT(0)
        }
        m if m == WM_SPARK_REHOTKEY => {
            rehook_hotkey(hwnd);
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 as usize == INDEX_TIMER_ID {
                if let Some(host) = unsafe { HOST_ARC.as_ref() } {
                    crate::index_watch::poll(host);
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn on_toggle() {
    // 1) Named event — UI 后台 WaitOne，唯一 toggle 通道（最稳）。
    //    不再走 pipe 广播 ui.toggle：event 与 pipe 是两条独立通道，pipe 延迟
    //    >300ms 到达时 UI 会收到第二次 toggle，把刚显示的窗口又关掉
    //    （表现为"第一次热键没反应、第二次才唤醒"）。toggle 语义单一化。
    crate::toggle_signal::signal_toggle();

    // 只在 UI 进程完全不存在时才拉起，避免每次热键都重复 spawn。
    // 若 UI 在运行但没连 pipe（演示模式），client_count 为 0 —— 此时
    // 再 spawn 会产生多个 UI 实例抢同一个 auto-reset 事件，热键时灵时不灵。
    if let Some(hub) = unsafe { HUB_ARC.as_ref() } {
        let n = hub.client_count();
        info!(ui_clients = n, "toggle → event");
        if n == 0 && !crate::is_ui_running() {
            try_spawn_ui();
        }
    } else {
        info!("toggle → event only (hub not ready)");
    }
}

fn try_spawn_ui() {
    // Look for spark near host or in known build output
    let Some(p) = crate::find_ui_exe() else {
        warn!("no UI clients and Spark.exe not found — start UI manually");
        return;
    };
    info!(path = %p.display(), "spawning spark");
    let mut cmd = std::process::Command::new(&p);
    if let Some(dir) = p.parent() {
        cmd.current_dir(dir);
    }
    if let Err(e) = cmd.spawn() {
        warn!(?e, "failed to spawn UI");
    }
}

fn toggle_hotkey_pause(hwnd: HWND) {
    let paused = HOTKEY_PAUSED.fetch_xor(true, Ordering::SeqCst);
    let now_paused = !paused;
    if let Some(host) = unsafe { HOST_ARC.as_ref() } {
        if let Ok(mut g) = host.lock() {
            g.set_hotkey_enabled(!now_paused);
            if now_paused {
                Hotkey::unregister(hwnd, HOTKEY_ID_TOGGLE);
                info!("hotkey paused");
            } else {
                match Hotkey::parse(&g.config.hotkey_toggle) {
                    Ok(hk) => {
                        if let Err(e) = hk.register(hwnd, HOTKEY_ID_TOGGLE) {
                            warn!(?e, "re-register hotkey failed");
                        } else {
                            info!("hotkey resumed");
                        }
                    }
                    Err(e) => warn!(?e, "parse hotkey"),
                }
            }
        }
    }
}

/// host.set_config 变更热键后重注册：先注销旧键再注册新键。
/// 热键暂停中（托盘"暂停热键"）只更新 config，注册留待恢复时进行。
fn rehook_hotkey(hwnd: HWND) {
    if HOTKEY_PAUSED.load(Ordering::SeqCst) {
        info!("hotkey paused; re-register deferred until resumed");
        return;
    }
    let Some(host) = (unsafe { HOST_ARC.as_ref() }) else {
        return;
    };
    let Ok(g) = host.lock() else { return };
    Hotkey::unregister(hwnd, HOTKEY_ID_TOGGLE);
    match Hotkey::parse(&g.config.hotkey_toggle) {
        Ok(hk) => match hk.register(hwnd, HOTKEY_ID_TOGGLE) {
            Ok(()) => info!(hotkey = %g.config.hotkey_toggle, "hotkey re-registered"),
            Err(e) => error!(?e, "hotkey re-register failed"),
        },
        Err(e) => error!(?e, raw = %g.config.hotkey_toggle, "invalid hotkey"),
    }
}
