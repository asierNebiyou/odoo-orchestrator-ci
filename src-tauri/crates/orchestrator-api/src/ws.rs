//! `/ws/events` streams every domain event as JSON, live, to whoever's
//! connected — the mechanism that lets the shell UI and the editor's context
//! extension react instead of poll.
//!
//! `/ws/logs` streams real process output the same way, on a **separate**
//! socket and a separate channel. Two reasons, both deliberate: log lines
//! arrive in bursts of hundreds while an Odoo boots and would starve the
//! event stream that drives the whole UI, and a screen that only wants to
//! know when a database appeared should not have to receive a traceback to
//! find out.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use serde::Deserialize;

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/ws/events", get(ws_handler)).route("/ws/logs", get(logs_ws_handler))
}

#[derive(Deserialize)]
struct WsAuthQuery {
    token: Option<String>,
}

/// The auth middleware exempts this path (browsers can't set headers on a WS
/// handshake), so the token check happens here instead, against a `?token=`
/// query param, before the connection is ever upgraded.
async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(q): Query<WsAuthQuery>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    match q.token {
        Some(t) if t == *state.token => ws.on_upgrade(move |socket| handle_socket(socket, state)).into_response(),
        _ => StatusCode::UNAUTHORIZED.into_response(),
    }
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    let mut rx = state.core.subscribe();
    loop {
        match rx.recv().await {
            Ok(envelope) => {
                let payload = match serde_json::to_string(&envelope) {
                    Ok(json) => json,
                    Err(err) => {
                        tracing::error!("failed to serialize event envelope: {err}");
                        continue;
                    }
                };
                if socket.send(Message::Text(payload)).await.is_err() {
                    // Client disconnected.
                    break;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!("ws/events subscriber lagged, skipped {skipped} events");
                continue;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// Same token-in-query handshake as `/ws/events` above, and for the same
/// reason: a browser cannot set headers on a WebSocket upgrade.
async fn logs_ws_handler(
    ws: WebSocketUpgrade,
    Query(q): Query<WsAuthQuery>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    match q.token {
        Some(t) if t == *state.token => ws.on_upgrade(move |socket| handle_log_socket(socket, state)).into_response(),
        _ => StatusCode::UNAUTHORIZED.into_response(),
    }
}

async fn handle_log_socket(mut socket: WebSocket, state: AppState) {
    let mut rx = state.core.subscribe_logs();
    loop {
        match rx.recv().await {
            Ok(line) => {
                let Ok(payload) = serde_json::to_string(&line) else { continue };
                if socket.send(Message::Text(payload)).await.is_err() {
                    break; // client disconnected
                }
            }
            // A lagging log reader is normal — a booting Odoo can outrun a
            // browser. Tell the client rather than silently dropping, so a
            // gap in a traceback is never mistaken for the end of it.
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                let notice = serde_json::json!({
                    "server_id": uuid::Uuid::nil(),
                    "at": chrono::Utc::now(),
                    "stream": "notice",
                    "line": format!("… {skipped} lines dropped: output arrived faster than this window could read it"),
                });
                if socket.send(Message::Text(notice.to_string())).await.is_err() {
                    break;
                }
                continue;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}
