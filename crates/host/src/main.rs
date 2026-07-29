//! spark-host entry. MVP: boot, seed index, answer local queries (no Win32 yet).

mod app;
mod config;

use anyhow::Result;
use clap::Parser;
use tracing::info;

#[derive(Debug, Parser)]
#[command(name = "spark-host", version, about = "Spark launcher host process")]
struct Cli {
    /// Print a one-shot search to stdout (dev helper) and exit
    #[arg(long)]
    query: Option<String>,

    /// Extra plugin directory to scan
    #[arg(long)]
    plugins_dir: Option<std::path::PathBuf>,

    /// Toggle UI (placeholder until IPC + UI exist)
    #[arg(long)]
    toggle: bool,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "spark_host=info,spark=info".into()),
        )
        .init();

    let cli = Cli::parse();
    let app = app::HostApp::bootstrap(cli.plugins_dir.as_deref())?;

    if let Some(q) = cli.query.as_deref() {
        let hits = app.search(q);
        println!("{}", serde_json::to_string_pretty(&hits)?);
        return Ok(());
    }

    if cli.toggle {
        info!("toggle requested (UI IPC not wired yet)");
        return Ok(());
    }

    info!(
        app = spark_core::APP_NAME,
        version = env!("CARGO_PKG_VERSION"),
        indexed = app.index_len(),
        plugins = app.plugin_count(),
        "spark-host ready (MVP console mode; Win32 hotkey/tray next)"
    );

    // MVP: stay alive briefly so scripts can smoke-test start; real loop is Win32 + tokio.
    info!("press Ctrl+C to exit");
    loop {
        std::thread::park();
    }
}
