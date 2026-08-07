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
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, PostQuitMessage,
    RegisterClassW, TranslateMessage, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, MSG, WINDOW_EX_STYLE,
    WM_DESTROY, WM_HOTKEY, WNDCLASSW, WS_OVERLAPPED,
};

static HOTKEY_PAUSED: AtomicBool = AtomicBool::new(false);
static mut HOST_PTR: Option<*const SharedHost> = None;
static mut HUB_PTR: Option<*const UiHub> = None;

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
        HOST_PTR = Some(Arc::as_ptr(&host) as *const SharedHost);
        HUB_PTR = Some(Arc::as_ptr(&hub) as *const UiHub);
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
    info!(
        ui_clients = hub.client_count(),
        "tray ready; message loop running"
    );

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    Hotkey::unregister(hwnd, HOTKEY_ID_TOGGLE);
    drop(tray);
    unsafe {
        HOST_PTR = None;
        HUB_PTR = None;
    }
    // keep hub alive until end
    drop(hub);
    Ok(())
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
                        PostQuitMessage(0);
                    }
                    _ => {}
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
    if let Some(ptr) = unsafe { HUB_PTR } {
        let hub = unsafe { &*ptr };
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
    let candidates = [
        "ui/Spark.UI/bin/Debug/net8.0-windows10.0.19041.0/win-x64/Spark.exe",
        "ui/Spark.UI/bin/Release/net8.0-windows10.0.19041.0/win-x64/Spark.exe",
        "Spark.exe",
    ];
    let mut path = None;
    for c in candidates {
        let p = std::path::PathBuf::from(c);
        if p.is_file() {
            path = Some(p);
            break;
        }
    }
    if path.is_none() {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let p = dir.join("Spark.exe");
                if p.is_file() {
                    path = Some(p);
                }
            }
        }
    }
    if let Some(p) = path {
        info!(path = %p.display(), "spawning spark");
        let mut cmd = std::process::Command::new(&p);
        if let Some(dir) = p.parent() {
            cmd.current_dir(dir);
        }
        if let Err(e) = cmd.spawn() {
            warn!(?e, "failed to spawn UI");
        }
    } else {
        warn!("no UI clients and Spark.exe not found — start UI manually");
    }
}

fn toggle_hotkey_pause(hwnd: HWND) {
    let paused = HOTKEY_PAUSED.fetch_xor(true, Ordering::SeqCst);
    let now_paused = !paused;
    if let Some(ptr) = unsafe { HOST_PTR } {
        let host = unsafe { &*ptr };
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
