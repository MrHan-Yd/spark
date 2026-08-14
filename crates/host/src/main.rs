//! spark-host: P0 — app index, launch, history, single-instance, hotkey, tray, IPC.

// 发布版不分配控制台窗口（安装器 [Run] / 快捷方式直接启动时不再弹黑窗）；
// debug 版保留控制台，终端里跑 dev_host.ps1 仍可看日志。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod builtins;
mod config;
mod hotkey;
mod index_watch;
mod ipc_server;
mod probe;
mod shell;
mod single_instance;
mod toggle_signal;
mod tray;
mod win_loop;

use anyhow::Result;
use clap::Parser;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use tracing::{info, warn};

#[derive(Debug, Parser)]
#[command(name = "spark-host", version, about = "Spark launcher host process")]
struct Cli {
    /// One-shot search → JSON stdout, then exit
    #[arg(long)]
    query: Option<String>,

    /// After --query, launch the first result
    #[arg(long)]
    launch: bool,

    /// Extra plugin directory to scan
    #[arg(long)]
    plugins_dir: Option<std::path::PathBuf>,

    /// Icon path for tray (.ico)
    #[arg(long)]
    icon: Option<std::path::PathBuf>,

    /// Allow a second process (dev only; skips mutex)
    #[arg(long)]
    allow_second: bool,

    /// Ask running host to toggle UI (via pipe)
    #[arg(long)]
    toggle: bool,

    /// Gracefully exit the running app (host + UI), e.g. before silent install
    #[arg(long)]
    exit: bool,

    /// Print spark window visibility, then exit (diagnostics)
    #[arg(long)]
    probe_ui: bool,

    /// Do not auto-spawn spark on start
    #[arg(long)]
    no_ui: bool,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "spark_host=info,spark=info".into()),
        )
        .init();

    let cli = Cli::parse();

    // One-shot query mode: no single-instance / no message loop
    if let Some(q) = cli.query.as_deref() {
        let mut host = app::HostApp::bootstrap(cli.plugins_dir.as_deref())?;
        if cli.launch {
            app::dev_invoke_first(&mut host, q)?;
        } else {
            let hits = host.search(q);
            println!("{}", serde_json::to_string_pretty(&hits)?);
        }
        return Ok(());
    }

    if cli.probe_ui {
        probe::print_ui_visible()?;
        return Ok(());
    }

    if cli.toggle {
        // 直接打事件（即使 pipe 失败，UI 也能醒）
        toggle_signal::signal_toggle();
        match ipc_server::send_toggle_to_running_host() {
            Ok(()) => info!("toggle sent (event + pipe)"),
            Err(e) => {
                info!(?e, "pipe toggle failed; event already signaled");
            }
        }
        return Ok(());
    }

    if cli.exit {
        // 优雅退出运行中的实例（走 host.exit → WM_SPARK_EXIT → 广播 ui.exit）
        match ipc_server::send_request_to_host("host.exit", serde_json::json!({})) {
            Ok(()) => info!("exit request sent"),
            Err(e) => warn!(?e, "exit request failed (host not running?)"),
        }
        return Ok(());
    }

    let _guard = if cli.allow_second {
        None
    } else {
        match single_instance::SingleInstance::acquire() {
            Ok(g) => Some(g),
            Err(e) => {
                info!(?e, "instance exists — forwarding toggle");
                match ipc_server::send_toggle_to_running_host() {
                    Ok(()) => info!("toggle forwarded"),
                    Err(err) => tracing::warn!(?err, "forward toggle failed"),
                }
                return Ok(());
            }
        }
    };

    let host = app::HostApp::bootstrap(cli.plugins_dir.as_deref())?;
    info!(
        app = spark_core::APP_NAME,
        version = env!("CARGO_PKG_VERSION"),
        indexed = host.index_len(),
        plugins = host.plugin_count(),
        hotkey = %host.config.hotkey_toggle,
        "spark-host starting"
    );

    let shared = app::share(host);
    let hub = ipc_server::spawn(shared.clone());
    // Give pipe a moment to bind before UI connects
    std::thread::sleep(std::time::Duration::from_millis(30));

    if !cli.no_ui {
        try_spawn_ui_on_boot();
    }

    let icon = cli.icon.or_else(find_default_icon);
    win_loop::run(shared, hub, icon)?;
    Ok(())
}

fn try_spawn_ui_on_boot() {
    // If UI already running, it will connect; else spawn.
    if is_ui_running() {
        info!("spark already running");
        return;
    }
    let Some(p) = find_ui_exe() else {
        info!("Spark.exe not found — start UI manually for IPC");
        return;
    };
    info!(path = %p.display(), "spawning spark on boot");
    let mut cmd = std::process::Command::new(&p);
    // 后台静默：UI 不弹窗，只连 IPC + 托盘常驻，用户按快捷键/点托盘才唤起
    cmd.arg("--hidden");
    if let Some(dir) = p.parent() {
        cmd.current_dir(dir);
    }
    match cmd.spawn() {
        Ok(_) => {}
        Err(e) => tracing::warn!(?e, "spawn UI failed"),
    }
}

/// host 可执行文件所在目录（安装位置无关的基准路径：用户装到哪都以它为准）。
pub(crate) fn exe_dir() -> Option<std::path::PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(std::path::Path::to_path_buf)
}

/// 定位 Spark.exe（UI 启动器）：
/// 1) host 同目录 —— 安装布局 `{app}\spark-host.exe` + `{app}\Spark.exe`（绝对路径）；
/// 2) 开发目录 —— cargo 产物 `ui/Spark.UI/bin/{Debug,Release}/…/Spark.exe`。
/// 不用裸文件名 "Spark.exe" 作 lpApplicationName：CreateProcess 对不含目录的相对名
/// 报 ERROR_INVALID_NAME(123)（安装器把 CWD 设为 {app} 时裸名会命中 is_file 但拉起失败）。
pub(crate) fn find_ui_exe() -> Option<std::path::PathBuf> {
    if let Some(dir) = exe_dir() {
        let p = dir.join("Spark.exe");
        if p.is_file() {
            return Some(p);
        }
    }
    for c in [
        "ui/Spark.UI/bin/Debug/net8.0-windows10.0.19041.0/win-x64/Spark.exe",
        "ui/Spark.UI/bin/Release/net8.0-windows10.0.19041.0/win-x64/Spark.exe",
    ] {
        let p = std::path::PathBuf::from(c);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

pub(crate) fn is_ui_running() -> bool {
    // Lightweight: try open pipe is not enough (host is the server).
    // Check process name.
    // CREATE_NO_WINDOW：host 是 GUI 子系统，直接 spawn tasklist（控制台程序）会
    // 新建一个控制台窗口闪一下（用户看到的"命令行黑框"），必须禁止创建窗口。
    std::process::Command::new("tasklist")
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .args(["/FI", "IMAGENAME eq Spark.exe", "/NH"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .to_lowercase()
                .contains("spark.exe")
        })
        .unwrap_or(false)
}

fn find_default_icon() -> Option<std::path::PathBuf> {
    // 安装布局：host 同目录的 spark.ico（或 Assets 子目录），安装位置无关。
    if let Some(dir) = exe_dir() {
        for name in ["spark.ico", "Assets/spark.ico"] {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    // 开发布局：仓库内相对 CWD 的候选。
    for c in [
        "ui/Spark.UI/Assets/spark.ico",
        "brand/spark.ico",
        "resources/spark.ico",
    ] {
        let p = std::path::PathBuf::from(c);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}
