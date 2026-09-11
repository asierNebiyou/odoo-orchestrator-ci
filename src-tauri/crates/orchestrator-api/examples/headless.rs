//! A headless stand-in for the desktop shell, used to exercise the real API
//! and the real UI against it without a windowing session.
//!
//! The shell binary itself is a genuine Tauri app now, so it can't build in
//! an environment with no GTK/webkit — but nothing about the API or the core
//! needs a window. This boots exactly what the shell boots (a real `Core`
//! over a real SQLite file, the real axum router, the real bearer token) and
//! prints the base URL and token, so a browser session can be pointed at it.
//!
//! Run: cargo run -p orchestrator-api --example headless -- <data-dir> <port>
use std::path::PathBuf;

use orchestrator_core::Core;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let data_dir = PathBuf::from(args.next().unwrap_or_else(|| "/tmp/orchestrator-headless".into()));
    let fixed_port: u16 = args.next().and_then(|p| p.parse().ok()).unwrap_or(0);

    // Without a subscriber every `tracing::error!` in the API goes
    // nowhere, so a 500 arrives at the client with no way to find out
    // what caused it. This is a debugging stand-in for the desktop shell;
    // being able to see the error is the whole point of it.
    tracing_subscriber::fmt().with_max_level(tracing::Level::INFO).init();

    std::fs::create_dir_all(&data_dir)?;
    let backups = data_dir.join("backups");
    let snapshots = data_dir.join("snapshots");
    std::fs::create_dir_all(&backups)?;
    std::fs::create_dir_all(&snapshots)?;

    let core = Core::open(data_dir.join("state.sqlite"))?;
    // Tell the core where its own folder is and where to look for Odoo
    // checkouts that are already on this machine — the same two things
    // the desktop shell will pass.
    let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| data_dir.clone());
    core.with_environment(&data_dir, vec![home]);
    let core_for_proxy = core.clone();
    let handle = orchestrator_api::serve_on(core, backups, snapshots, fixed_port).await?;

    // The hostname proxy — `acme.odoo:<port>` rather than
    // `acme.localhost:8069`. A high port by default: :80 needs privileges
    // this shouldn't quietly take.
    let proxy_port: u16 = std::env::var("ORCHESTRATOR_PROXY_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8080);
    match orchestrator_api::serve_proxy(core_for_proxy, proxy_port).await {
        Ok(proxy) => println!("ORCHESTRATOR_PROXY=http://{}", proxy.addr),
        Err(err) => eprintln!("proxy failed to start: {err}"),
    }

    println!("ORCHESTRATOR_BASE_URL={}", handle.base_url());
    println!("ORCHESTRATOR_TOKEN={}", handle.token);

    // Park forever; the caller kills the process when it's done.
    std::future::pending::<()>().await;
    Ok(())
}
