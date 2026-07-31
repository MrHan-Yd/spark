//! spark-host: P0 — app index, launch, history, single-instance, hotkey, tray, IPC.

mod app;
mod config;
mod hotkey;
mod ipc_server;
mod shell;
mod single_instance;
mod toggle_signal;
mod tray;
mod win_loop;

use anyhow::Result;
use clap::Parser;
use tracing::info;

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

    /// Do not auto-spawn spark-ui on start
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
        info!("spark-ui already running");
        return;
    }
    let candidates = [
        "ui/Spark.UI/bin/Debug/net8.0-windows10.0.19041.0/win-x64/spark-ui.exe",
        "ui/Spark.UI/bin/Release/net8.0-windows10.0.19041.0/win-x64/spark-ui.exe",
    ];
    for c in candidates {
        let p = std::path::PathBuf::from(c);
        if p.is_file() {
            info!(path = %p.display(), "spawning spark-ui on boot");
            let mut cmd = std::process::Command::new(&p);
            if let Some(dir) = p.parent() {
                cmd.current_dir(dir);
            }
            match cmd.spawn() {
                Ok(_) => return,
                Err(e) => tracing::warn!(?e, "spawn UI failed"),
            }
        }
    }
    info!("spark-ui.exe not found — start UI manually for IPC");
}

pub(crate) fn is_ui_running() -> bool {
    // Lightweight: try open pipe is not enough (host is the server).
    // Check process name.
    std::process::Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq spark-ui.exe", "/NH"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .to_lowercase()
                .contains("spark-ui")
        })
        .unwrap_or(false)
}

fn find_default_icon() -> Option<std::path::PathBuf> {
    let candidates = [
        "ui/Spark.UI/Assets/spark.ico",
        "brand/spark.ico",
        "resources/spark.ico",
    ];
    for c in candidates {
        let p = std::path::PathBuf::from(c);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("spark.ico");
            if p.is_file() {
                return Some(p);
            }
            let p = dir.join("Assets").join("spark.ico");
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}
