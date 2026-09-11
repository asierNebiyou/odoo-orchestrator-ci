//! The proxy against real HTTP servers, covering both supported shapes at
//! once: one Odoo serving several databases, and a second Odoo serving one
//! of its own.
//!
//! State is written through `Db` directly rather than `Core`'s methods
//! because `create_database` does real `createdb` work against a real
//! cluster — irrelevant to routing, and not something a routing test
//! should need Postgres for.

use std::net::SocketAddr;

use axum::extract::Request;
use axum::routing::any;
use orchestrator_core::model::{Database, OdooServer, PostgresInstance, ServerState};
use orchestrator_core::{Core, Db};
use tokio::net::TcpListener;

/// A stand-in Odoo: answers everything with the label it was built with
/// and the Host header it actually received, which is what proves the
/// rewrite happened.
async fn fake_odoo(label: &'static str) -> SocketAddr {
    let app = axum::Router::new().fallback(any(move |req: Request| async move {
        let host = req
            .headers()
            .get(axum::http::header::HOST)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("<none>")
            .to_string();
        format!("{label}|{host}|{}", req.uri().path())
    }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

async fn get(port: u16, host: &str, path: &str) -> (u16, String) {
    let client = reqwest_lite::get(port, host, path).await;
    client
}

/// Minimal HTTP/1.1 GET so this test needs no HTTP client dependency.
mod reqwest_lite {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    pub async fn get(port: u16, host: &str, path: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
        stream.write_all(req.as_bytes()).await.unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await.unwrap();
        let text = String::from_utf8_lossy(&raw).to_string();
        let status = text
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(0);
        let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
        (status, body)
    }
}

#[tokio::test]
async fn one_odoo_with_several_databases_and_one_odoo_of_its_own_route_the_same_way() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("state.sqlite");

    let shared = fake_odoo("shared").await;
    let alone = fake_odoo("alone").await;

    // Two Odoos: one holding two databases, one holding a single database.
    let db = Db::open(&db_path).unwrap();
    let pg = PostgresInstance::new("test", "16", 5432, dir.path().to_string_lossy().to_string());
    db.insert_postgres_instance(&pg).unwrap();

    let mut acme_server = OdooServer::new("Acme 17", "17.0", shared.port(), pg.id);
    acme_server.state = ServerState::Running;
    db.insert_server(&acme_server).unwrap();
    db.update_server_state(acme_server.id, &ServerState::Running).unwrap();

    let mut north_server = OdooServer::new("Northwind 16", "16.0", alone.port(), pg.id);
    north_server.state = ServerState::Running;
    db.insert_server(&north_server).unwrap();
    db.update_server_state(north_server.id, &ServerState::Running).unwrap();

    let acme = Database::new(acme_server.id, "acme");
    let acme_test = Database::new(acme_server.id, "acme_test");
    let northwind = Database::new(north_server.id, "northwind");
    for d in [&acme, &acme_test, &northwind] {
        db.insert_database(d).unwrap();
    }
    drop(db);

    let core = Core::open(&db_path).unwrap();
    let proxy = orchestrator_api::serve_proxy(core.clone(), 0).await.unwrap();
    let port = proxy.addr.port();

    // Both databases on the shared Odoo reach the same process, and each
    // arrives with its own name in the Host header — which is the whole
    // mechanism Odoo's dbfilter uses to tell them apart.
    let (status, body) = get(port, "acme.localhost", "/web/login").await;
    assert_eq!(status, 200);
    assert_eq!(body, "shared|acme.localhost|/web/login");

    let (_, body) = get(port, "acme_test.localhost", "/").await;
    assert_eq!(body, "shared|acme_test.localhost|/", "same process, different database");

    // The Odoo with a database to itself is a different port entirely, and
    // the caller never had to know that.
    let (_, body) = get(port, "northwind.localhost", "/").await;
    assert_eq!(body, "alone|northwind.localhost|/");

    // A port in the Host header must not change the routing.
    let (_, body) = get(port, "acme.localhost:8080", "/").await;
    assert_eq!(body, "shared|acme.localhost|/");
}

#[tokio::test]
async fn a_custom_domain_reaches_its_database_without_odoo_knowing_the_domain() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("state.sqlite");
    let backend = fake_odoo("backend").await;

    let db = Db::open(&db_path).unwrap();
    let pg = PostgresInstance::new("test", "16", 5432, dir.path().to_string_lossy().to_string());
    db.insert_postgres_instance(&pg).unwrap();
    let server = OdooServer::new("Acme", "17.0", backend.port(), pg.id);
    db.insert_server(&server).unwrap();
    db.update_server_state(server.id, &ServerState::Running).unwrap();
    let database = Database::new(server.id, "acme_prod");
    db.insert_database(&database).unwrap();
    drop(db);

    let core = Core::open(&db_path).unwrap();
    core.set_database_domain(database.id, Some("books.acme.test")).unwrap();

    let proxy = orchestrator_api::serve_proxy(core.clone(), 0).await.unwrap();
    let port = proxy.addr.port();

    // The browser used a domain that has nothing to do with the database's
    // name; Odoo is still told `acme_prod.localhost`, so its own dbfilter
    // selects the right database. That decoupling is the point.
    let (status, body) = get(port, "books.acme.test", "/").await;
    assert_eq!(status, 200);
    assert_eq!(body, "backend|acme_prod.localhost|/");

    // And the default name stops working once it has been overridden —
    // one database answers on exactly one hostname.
    let (status, _) = get(port, "acme_prod.localhost", "/").await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn two_databases_cannot_be_given_the_same_domain() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("state.sqlite");

    let db = Db::open(&db_path).unwrap();
    let pg = PostgresInstance::new("test", "16", 5432, dir.path().to_string_lossy().to_string());
    db.insert_postgres_instance(&pg).unwrap();
    let server = OdooServer::new("Acme", "17.0", 8069, pg.id);
    db.insert_server(&server).unwrap();
    let one = Database::new(server.id, "one");
    let two = Database::new(server.id, "two");
    db.insert_database(&one).unwrap();
    db.insert_database(&two).unwrap();
    drop(db);

    let core = Core::open(&db_path).unwrap();
    core.set_database_domain(one.id, Some("shared.localhost")).unwrap();

    // Silently letting this through would mean logging into whichever
    // database happened to be listed first — the exact class of mistake
    // this product exists to prevent.
    let clash = core.set_database_domain(two.id, Some("shared.localhost"));
    assert!(clash.is_err(), "a second database must not be allowed onto a taken domain");

    // Re-setting the same domain on the database that already has it is
    // not a clash.
    assert!(core.set_database_domain(one.id, Some("shared.localhost")).is_ok());
}
