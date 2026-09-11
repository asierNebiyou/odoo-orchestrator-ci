//! Bearer-token check on every route except `/health`. This is the whole
//! "local API as an attack surface" mitigation flagged as a risk in
//! `odoo-orchestrator-technical-design.md` — loopback binding alone isn't
//! enough since any other local process can still reach a loopback port.

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;

use crate::AppState;

pub async fn require_token(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    // /health needs no auth. The WebSocket routes are exempted from this
    // header-based check too: browsers cannot set custom headers on a
    // WebSocket handshake, so each takes its token as a query param and
    // checks it itself in ws.rs. Exempted here, **not** unauthenticated —
    // the check moves, it does not disappear.
    let path = req.uri().path();
    if path == "/health" || path == "/ws/events" || path == "/ws/logs" {
        return Ok(next.run(req).await);
    }

    let provided = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    match provided {
        Some(token) if token == state.token.as_str() => Ok(next.run(req).await),
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}
