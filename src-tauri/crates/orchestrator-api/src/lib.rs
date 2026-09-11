//! Local HTTP+WebSocket API over `orchestrator-core`. Binds to 127.0.0.1 on
//! an OS-assigned port and requires a per-launch bearer token on every route
//! except `/health` — the loopback-plus-token model from the technical-design
//! doc. The shell UI, the openvscode-server sidecar's context extension, and
//! any future plugin all talk to this instead of getting direct bindings.

mod auth;
mod proxy;
mod routes;

pub use proxy::{serve as serve_proxy, ProxyHandle};
mod ws;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use orchestrator_core::Core;
use rand::Rng;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

#[derive(Clone)]
pub struct AppState {
    pub core: Core,
    pub token: Arc<String>,
    /// Where `backup_database` writes its `pg_dump` output. Kept as a plain
    /// directory rather than something core.rs decides, since it's a shell
    /// concern (part of the app's data dir layout), not a domain one.
    pub backups_dir: Arc<PathBuf>,
    /// Where `create_snapshot` writes a filestore snapshot's hardlinked
    /// copy, when one is taken — same posture as `backups_dir` above.
    pub snapshots_dir: Arc<PathBuf>,
}

pub struct ApiHandle {
    pub addr: SocketAddr,
    pub token: String,
    _server: JoinHandle<()>,
}

impl ApiHandle {
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

fn generate_token() -> String {
    let mut rng = rand::thread_rng();
    (0..32)
        .map(|_| {
            let charset = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
            charset[rng.gen_range(0..charset.len())] as char
        })
        .collect()
}

pub fn build_router(state: AppState) -> Router {
    // Permissive CORS is fine here: this server only ever binds to loopback,
    // and every non-/health route is still gated by the bearer token below —
    // CORS is about which *browser pages* may read the response, not who can
    // reach the socket.
    let cors = tower_http::cors::CorsLayer::permissive();

    Router::new()
        .merge(routes::router())
        .merge(ws::router())
        .layer(axum::middleware::from_fn_with_state(state.clone(), auth::require_token))
        .layer(cors)
        .with_state(state)
}

/// Bind to 127.0.0.1 on an OS-assigned port and start serving in the
/// background. Returns immediately with the bound address and token.
pub async fn serve(core: Core, backups_dir: PathBuf, snapshots_dir: PathBuf) -> std::io::Result<ApiHandle> {
    serve_on(core, backups_dir, snapshots_dir, 0).await
}

/// As `serve`, but binds a caller-chosen loopback port. `0` keeps the
/// OS-assigned behaviour `serve` has always had — a fixed port exists only
/// so a test harness can point a browser at a known URL; the shell itself
/// should keep using `serve`.
pub async fn serve_on(
    core: Core,
    backups_dir: PathBuf,
    snapshots_dir: PathBuf,
    port: u16,
) -> std::io::Result<ApiHandle> {
    let token = generate_token();
    let state = AppState { core, token: Arc::new(token.clone()), backups_dir: Arc::new(backups_dir), snapshots_dir: Arc::new(snapshots_dir) };
    let app = build_router(state);

    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    let addr = listener.local_addr()?;

    let server = tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app).await {
            tracing::error!("orchestrator-api server error: {err}");
        }
    });

    Ok(ApiHandle { addr, token, _server: server })
}
