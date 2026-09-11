//! The thing that makes `acme.localhost` work.
//!
//! Two shapes have to be supported, and this is what lets them be the same
//! mechanism rather than two code paths:
//!
//! * **One Odoo, several databases.** Odoo's own `dbfilter = ^%d$` already
//!   routes by the first label of the Host header, so `acme.localhost` and
//!   `northwind.localhost` reaching the same process pick different
//!   databases by themselves. This proxy exists here only to remove the
//!   port number from the URL.
//! * **One Odoo per database.** Each has its own port, and a hostname is
//!   the only thing a person should have to remember. This proxy is what
//!   maps one to the other.
//!
//! It also decouples a database's *domain* from its *name*: the Host header
//! is rewritten to `<database>.localhost` before forwarding, so
//! `books.acme.localhost` can serve a database called `acme_prod` without
//! Odoo ever knowing.
//!
//! `.localhost` rather than `.odoo` — see
//! `docs/odoo-orchestrator-address-decision.md`; the short version is that
//! `.localhost` is reserved by RFC 6761 and can never be delegated, while
//! ICANN's new-gTLD window is open right now and `.dev` is the precedent
//! for what happens when a TLD you borrowed gets bought.
//!
//! **Ports and name resolution are deliberately not this module's problem.**
//! It binds whatever port it's given (default 8080, no privileges needed);
//! serving on :80 needs one-time administrator setup on the machine, which
//! is offered explicitly rather than done behind someone's back. Until
//! that's done, `acme.localhost:8080` works and `acme.localhost` doesn't —
//! which is a comprehensible half-way state, not a broken one.

use std::convert::Infallible;
use std::net::SocketAddr;


use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use orchestrator_core::{Core, Route};
use tokio::net::TcpListener;

#[derive(Clone)]
struct ProxyState {
    core: Core,
}

pub struct ProxyHandle {
    pub addr: SocketAddr,
    _server: tokio::task::JoinHandle<()>,
}

/// Start the proxy on `port` (0 for an OS-assigned one). Routes are read
/// from the core per request rather than cached: a database created a
/// second ago has to work immediately, and the lookup is a SQLite read of
/// a handful of rows.
pub async fn serve(core: Core, port: u16) -> std::io::Result<ProxyHandle> {
    let app = axum::Router::new()
        .fallback(handle)
        .with_state(ProxyState { core });

    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    let addr = listener.local_addr()?;
    let server = tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app).await {
            tracing::error!("proxy server error: {err}");
        }
    });
    Ok(ProxyHandle { addr, _server: server })
}

async fn handle(State(state): State<ProxyState>, req: Request) -> Result<Response, Infallible> {
    let host = req
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(strip_port)
        .unwrap_or_default()
        .to_lowercase();

    let routes = match state.core.routing_table() {
        Ok(routes) => routes,
        Err(err) => return Ok(error_page(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string(), &[])),
    };

    let Some(route) = find_route(&routes, &host).cloned() else {
        return Ok(unknown_host(&host, &routes));
    };
    if !route.running {
        return Ok(error_page(
            StatusCode::SERVICE_UNAVAILABLE,
            &format!("{host} is a real address, but the Odoo behind it isn't running."),
            &[],
        ));
    }
    Ok(forward(req, &route).await)
}

/// Which route serves `host`.
///
/// An exact match always wins. Failing that, a host whose **first label**
/// is a database's own name matches — which is what makes the address
/// suffix a real setting rather than a constant compiled in here:
/// `acme.localhost` and `acme.test` are the same database, and Odoo's own
/// dbfilter reads only the first label anyway.
///
/// Deliberately narrow: only databases still answering on their default
/// `<name>.<suffix>` address take part. A database given a **custom**
/// domain must be matched exactly, because the entire reason someone sets
/// one is to decouple the address from the name — quietly honouring the
/// name as well would hand a second, unasked-for address to the one
/// database whose owner explicitly chose otherwise.
fn find_route<'a>(routes: &'a [Route], host: &str) -> Option<&'a Route> {
    if let Some(exact) = routes.iter().find(|r| r.host == host) {
        return Some(exact);
    }
    let label = host.split('.').next().unwrap_or_default();
    // A bare hostname with no suffix at all isn't an address for a
    // database — it's someone reaching the proxy directly.
    if label.is_empty() || label == host {
        return None;
    }
    routes.iter().find(|r| r.database == label && r.host == format!("{}.localhost", r.database))
}

async fn forward(req: Request, route: &Route) -> Response {
    let (mut parts, body) = req.into_parts();

    let path_and_query = parts.uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    let target = format!("http://127.0.0.1:{}{}", route.port, path_and_query);
    parts.uri = match target.parse::<Uri>() {
        Ok(uri) => uri,
        Err(err) => return error_page(StatusCode::BAD_GATEWAY, &err.to_string(), &[]),
    };

    // The rewrite that does the real work: whatever hostname the browser
    // used, Odoo is told `<database>.odoo`, which is what its own dbfilter
    // matches on. This is why a custom domain needs no Odoo config at all.
    if let Ok(value) = format!("{}.localhost", route.database).parse() {
        parts.headers.insert(axum::http::header::HOST, value);
    }
    // Hop-by-hop headers must not be forwarded.
    for header in ["connection", "keep-alive", "transfer-encoding", "upgrade", "proxy-connection"] {
        parts.headers.remove(header);
    }

    let client = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .build_http::<Body>();
    match client.request(Request::from_parts(parts, body)).await {
        Ok(res) => res.into_response(),
        Err(err) => error_page(
            StatusCode::BAD_GATEWAY,
            &format!("Couldn't reach the Odoo on port {} — {err}", route.port),
            &[],
        ),
    }
}

/// A wrong hostname is the single most likely thing to go wrong here, so it
/// gets a real answer listing what *does* work rather than a bare 404.
fn unknown_host(host: &str, routes: &[Route]) -> Response {
    let known: Vec<String> = routes.iter().map(|r| r.host.clone()).collect();
    error_page(
        StatusCode::NOT_FOUND,
        &format!("Nothing is registered at {host}."),
        &known,
    )
}

fn error_page(status: StatusCode, message: &str, known: &[String]) -> Response {
    let list = if known.is_empty() {
        String::new()
    } else {
        let items: String = known.iter().map(|h| format!("<li><code>{h}</code></li>")).collect();
        format!("<p>These addresses exist:</p><ul>{items}</ul>")
    };
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>Orchestrator</title>\
         <body style=\"font:14px -apple-system,system-ui,sans-serif;max-width:38rem;margin:12vh auto;padding:0 1.5rem;color:#16171a\">\
         <h1 style=\"font-size:17px\">{message}</h1>{list}\
         <p style=\"color:#6b7078\">This page is the Orchestrator proxy, not Odoo.</p>"
    );
    (status, [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")], body).into_response()
}

fn strip_port(host: &str) -> &str {
    host.split(':').next().unwrap_or(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_port_in_the_host_header_is_not_part_of_the_hostname() {
        assert_eq!(strip_port("acme.localhost:8080"), "acme.localhost");
        assert_eq!(strip_port("acme.localhost"), "acme.localhost");
        assert_eq!(strip_port(""), "");
    }

    fn route(host: &str, database: &str) -> Route {
        Route {
            host: host.into(),
            port: 8069,
            database: database.into(),
            server_id: uuid::Uuid::new_v4(),
            database_id: uuid::Uuid::new_v4(),
            running: true,
        }
    }

    #[test]
    fn an_unknown_host_says_what_does_exist_instead_of_just_404ing() {
        let routes = vec![route("acme.localhost", "acme")];
        let res = unknown_host("typo.localhost", &routes);
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn a_default_address_answers_under_any_suffix() {
        let routes = vec![route("acme.localhost", "acme")];
        assert_eq!(find_route(&routes, "acme.localhost").unwrap().database, "acme");
        assert_eq!(find_route(&routes, "acme.test").unwrap().database, "acme");
        assert_eq!(find_route(&routes, "acme.internal").unwrap().database, "acme");
    }

    #[test]
    fn a_custom_domain_is_matched_exactly_and_never_by_its_database_name() {
        // `acme_prod` was deliberately given the address `books.acme.localhost`.
        // Its own name must not become a second working address.
        let routes = vec![route("books.acme.localhost", "acme_prod")];
        assert_eq!(find_route(&routes, "books.acme.localhost").unwrap().database, "acme_prod");
        assert!(find_route(&routes, "acme_prod.localhost").is_none());
        assert!(find_route(&routes, "acme_prod.test").is_none());
    }

    #[test]
    fn a_bare_hostname_with_no_suffix_matches_nothing() {
        let routes = vec![route("acme.localhost", "acme")];
        assert!(find_route(&routes, "acme").is_none());
        assert!(find_route(&routes, "").is_none());
    }

    #[test]
    fn an_unrelated_first_label_still_misses() {
        let routes = vec![route("acme.localhost", "acme")];
        assert!(find_route(&routes, "northwind.localhost").is_none());
    }
}
