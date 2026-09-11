//! Real Postgres database lifecycle operations — task 2.3. Deliberately
//! generic Postgres admin (`createdb`/`dropdb`/duplicate-via-template/
//! `pg_dump`/restore), not Odoo-specific: Odoo's own "Duplicate database"
//! and "Back up" buttons are themselves thin wrappers over exactly these
//! same operations, per `odoo-orchestrator-runtime-architecture.md`'s
//! researched gotchas, which this module implements directly:
//!
//! - **Duplicating a database** force-disconnects every other session on
//!   the source first, then runs `CREATE DATABASE … TEMPLATE …` — Postgres
//!   refuses to use a database as a template while anything else is
//!   connected to it.
//! - **Backup format matters**: Odoo's `zip` format is `dump.sql` +
//!   `manifest.json` + the filestore; its `dump` format is a raw
//!   `pg_dump`, filestore **not** included. This module implements the
//!   `pg_dump` half (real, tested against a real database). The filestore
//!   side of `zip` is deliberately **not** implemented — there is no real
//!   Odoo filestore anywhere in this sandbox to copy or validate against
//!   (see `odoo.rs`'s module doc comment for the matching reasoning: this
//!   project doesn't build against a shape it can't verify).
//!
//! Verified against a real running Postgres instance, not mocked: create a
//! database, insert real data via `psql`, duplicate it and confirm the data
//! carried over, drop the original, dump the duplicate to a real `.sql`
//! file, drop it too, restore into a brand-new database from that dump
//! file, and confirm the data survived the entire round trip.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tokio::task::spawn_blocking;

#[derive(Debug, thiserror::Error)]
pub enum PgAdminError {
    #[error("couldn't run {command}: {source}")]
    Spawn { command: String, source: std::io::Error },
    #[error("{command} exited with {exit_code:?}: {stderr}")]
    CommandFailed { command: String, exit_code: Option<i32>, stderr: String },
}

/// Where and how to reach a running Postgres instance's client tools.
/// `host` is the *directory* containing its Unix socket — Postgres client
/// tools accept a socket directory as `-h`, matching exactly how
/// `postgres.rs` starts the server (`-k <data_dir>`) and how its own
/// `pg_isready` check connects.
#[derive(Debug, Clone)]
pub struct PgConnInfo {
    pub bin_dir: PathBuf,
    pub host: PathBuf,
    pub port: u16,
    pub user: String,
}

impl PgConnInfo {
    pub fn new(bin_dir: impl Into<PathBuf>, host: impl Into<PathBuf>, port: u16) -> Self {
        Self { bin_dir: bin_dir.into(), host: host.into(), port, user: "postgres".to_string() }
    }

    fn apply_common_args(&self, cmd: &mut Command) {
        cmd.arg("-h").arg(&self.host).arg("-p").arg(self.port.to_string()).arg("-U").arg(&self.user);
    }
}

/// Runs `sql` against `database` and returns the single value of its first
/// row/column, or `None` if it returned no rows — the same minimal
/// `psql -tAc` shape `database_exists` already uses below, generalized to
/// an arbitrary caller-supplied query. Exists for one real, narrow reason:
/// task 2.4's module-upgrade command needs to report the module's real
/// post-upgrade `ir_module_module.latest_version`, not a fabricated one —
/// this project's "no fake progress" rule applies to that number too.
pub async fn query_scalar(conn: &PgConnInfo, database: &str, sql: &str) -> Result<Option<String>, PgAdminError> {
    let conn = conn.clone();
    let database = database.to_string();
    let sql = sql.to_string();
    spawn_blocking(move || {
        let mut cmd = Command::new(conn.bin_dir.join("psql"));
        conn.apply_common_args(&mut cmd);
        cmd.arg(&database).arg("-tAc").arg(&sql);
        let output = run(cmd, "psql (query_scalar)")?;
        check_success(&output, "psql (query_scalar)")?;
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(if text.is_empty() { None } else { Some(text) })
    })
    .await
    .expect("query_scalar blocking task panicked")
}

/// Runs `sql` against `database` and returns every row as a vector of
/// column values (unaligned, tab-separated `psql -Atc`, split back apart on
/// the same delimiter) — `query_scalar`'s multi-row, multi-column sibling.
/// Exists for the same real, narrow reason: task 2.5's Modules-screen
/// currency instrument needs to read every row of a database's real
/// `ir_module_module` (name/state/version columns), not just one value.
/// Runs one statement against `database`, discarding any output. For SQL
/// that changes rows rather than reading them — `query_scalar` would hand
/// back psql's "INSERT 0 1" tag and invite a caller to treat it as data.
pub async fn execute(conn: &PgConnInfo, database: &str, sql: &str) -> Result<(), PgAdminError> {
    query_scalar(conn, database, sql).await.map(|_| ())
}

pub async fn query_rows(conn: &PgConnInfo, database: &str, sql: &str) -> Result<Vec<Vec<String>>, PgAdminError> {
    let conn = conn.clone();
    let database = database.to_string();
    let sql = sql.to_string();
    spawn_blocking(move || {
        let mut cmd = Command::new(conn.bin_dir.join("psql"));
        conn.apply_common_args(&mut cmd);
        cmd.arg(&database).arg("-F").arg("\t").arg("-Atc").arg(&sql);
        let output = run(cmd, "psql (query_rows)")?;
        check_success(&output, "psql (query_rows)")?;
        let text = String::from_utf8_lossy(&output.stdout);
        Ok(text.lines().filter(|l| !l.is_empty()).map(|l| l.split('\t').map(str::to_string).collect()).collect())
    })
    .await
    .expect("query_rows blocking task panicked")
}

pub async fn database_exists(conn: &PgConnInfo, name: &str) -> Result<bool, PgAdminError> {
    let conn = conn.clone();
    let name = name.to_string();
    spawn_blocking(move || {
        let mut cmd = Command::new(conn.bin_dir.join("psql"));
        conn.apply_common_args(&mut cmd);
        cmd.arg("-tAc")
            .arg(format!("SELECT 1 FROM pg_database WHERE datname = '{}'", escape_sql_literal(&name)))
            .arg("postgres"); // connects to the maintenance db, not the target
        let output = run(cmd, "psql (database_exists)")?;
        check_success(&output, "psql (database_exists)")?;
        Ok(String::from_utf8_lossy(&output.stdout).trim() == "1")
    })
    .await
    .expect("psql blocking task panicked")
}

pub async fn create_database(conn: &PgConnInfo, name: &str) -> Result<(), PgAdminError> {
    let conn = conn.clone();
    let name = name.to_string();
    spawn_blocking(move || {
        let mut cmd = Command::new(conn.bin_dir.join("createdb"));
        conn.apply_common_args(&mut cmd);
        // Belt and braces alongside `initdb --encoding=UTF8` (see
        // postgres.rs): Odoo requires a UTF8 database, and a cluster this
        // app didn't create — one the user adopted, or an existing local
        // PostgreSQL — may well have a non-UTF8 `template1`. Naming
        // `template0` is what makes an explicit encoding legal at all;
        // `createdb` refuses `--encoding` against `template1` unless the
        // two already agree.
        cmd.arg("--encoding=UTF8").arg("--template=template0").arg("--lc-collate=C").arg("--lc-ctype=C");
        cmd.arg(&name);
        let output = run(cmd, "createdb")?;
        check_success(&output, "createdb")
    })
    .await
    .expect("createdb blocking task panicked")
}

/// `--if-exists` makes this idempotent, matching the "no-op rather than an
/// error" posture `postgres.rs`/`odoo.rs`'s own `stop()` calls use for an
/// already-absent target.
pub async fn drop_database(conn: &PgConnInfo, name: &str) -> Result<(), PgAdminError> {
    let conn = conn.clone();
    let name = name.to_string();
    spawn_blocking(move || {
        let mut cmd = Command::new(conn.bin_dir.join("dropdb"));
        conn.apply_common_args(&mut cmd);
        cmd.arg("--if-exists").arg(&name);
        let output = run(cmd, "dropdb")?;
        check_success(&output, "dropdb")
    })
    .await
    .expect("dropdb blocking task panicked")
}

/// Ensures a login role named `role` exists with `CREATEDB` (but not
/// superuser) — idempotent, matching `drop_database`'s "no-op rather than
/// an error" posture for the state that already holds. This exists for one
/// real reason: real Odoo hard-refuses to start (`"Using the database user
/// 'postgres' is a security risk, aborting."`) when its own `db_user` is
/// the cluster's superuser role — which every other operation in this
/// module deliberately *does* connect as (see `PgConnInfo::new`'s
/// `"postgres"` default), since this project's own cluster has exactly one
/// admin role. So Odoo needs its own, separate, non-superuser role; this is
/// how it gets created.
pub async fn ensure_role(conn: &PgConnInfo, role: &str) -> Result<(), PgAdminError> {
    let conn = conn.clone();
    let role = role.to_string();
    spawn_blocking(move || {
        let mut exists_cmd = Command::new(conn.bin_dir.join("psql"));
        conn.apply_common_args(&mut exists_cmd);
        exists_cmd
            .arg("-tAc")
            .arg(format!("SELECT 1 FROM pg_roles WHERE rolname = '{}'", escape_sql_literal(&role)))
            .arg("postgres");
        let output = run(exists_cmd, "psql (role exists)")?;
        check_success(&output, "psql (role exists)")?;
        if String::from_utf8_lossy(&output.stdout).trim() == "1" {
            return Ok(());
        }

        let mut create_cmd = Command::new(conn.bin_dir.join("psql"));
        conn.apply_common_args(&mut create_cmd);
        create_cmd.arg("postgres").arg("-c").arg(format!("CREATE ROLE \"{}\" LOGIN CREATEDB", escape_identifier(&role)));
        let output = run(create_cmd, "psql (CREATE ROLE)")?;
        check_success(&output, "psql (CREATE ROLE)")
    })
    .await
    .expect("ensure_role blocking task panicked")
}

/// Grants `role` the privileges it needs to create tables in `database`'s
/// own `public` schema — idempotent (a repeat `GRANT` is a no-op, not an
/// error). Exists for one real, version-specific reason: Postgres 15
/// revoked the `CREATE`-on-`public`-for-every-role default every earlier
/// version had, so a fresh non-superuser role like `ensure_role`'s "odoo"
/// can authenticate fine but then fail with `permission denied for schema
/// public` the moment Odoo tries to create its first table — a real,
/// commonly-hit-in-the-wild Odoo/Postgres-15+ incompatibility, not
/// something specific to this project's own cluster setup.
pub async fn grant_schema_privileges(conn: &PgConnInfo, database: &str, role: &str) -> Result<(), PgAdminError> {
    let conn = conn.clone();
    let (database, role) = (database.to_string(), role.to_string());
    spawn_blocking(move || {
        let mut cmd = Command::new(conn.bin_dir.join("psql"));
        conn.apply_common_args(&mut cmd);
        cmd.arg(&database).arg("-c").arg(format!("GRANT ALL ON SCHEMA public TO \"{}\"", escape_identifier(&role)));
        let output = run(cmd, "psql (GRANT ON SCHEMA public)")?;
        check_success(&output, "psql (GRANT ON SCHEMA public)")
    })
    .await
    .expect("grant_schema_privileges blocking task panicked")
}

/// Force-disconnects every other session on `source`, then runs `CREATE
/// DATABASE target TEMPLATE source` — see the module doc comment for why
/// both steps are required.
pub async fn duplicate_database(conn: &PgConnInfo, source: &str, target: &str) -> Result<(), PgAdminError> {
    let conn = conn.clone();
    let (source, target) = (source.to_string(), target.to_string());
    spawn_blocking(move || {
        terminate_backends(&conn, &source)?;
        let mut cmd = Command::new(conn.bin_dir.join("psql"));
        conn.apply_common_args(&mut cmd);
        cmd.arg("postgres").arg("-c").arg(format!(
            "CREATE DATABASE \"{}\" TEMPLATE \"{}\"",
            escape_identifier(&target),
            escape_identifier(&source)
        ));
        let output = run(cmd, "psql (CREATE DATABASE ... TEMPLATE)")?;
        check_success(&output, "psql (CREATE DATABASE ... TEMPLATE)")
    })
    .await
    .expect("duplicate_database blocking task panicked")
}

fn terminate_backends(conn: &PgConnInfo, database: &str) -> Result<(), PgAdminError> {
    let mut cmd = Command::new(conn.bin_dir.join("psql"));
    conn.apply_common_args(&mut cmd);
    cmd.arg("postgres").arg("-c").arg(format!(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '{}' AND pid <> pg_backend_pid()",
        escape_sql_literal(database)
    ));
    let output = run(cmd, "psql (terminate backends)")?;
    check_success(&output, "psql (terminate backends)")
}

/// Plain-SQL dump via `pg_dump` — the "dump" format
/// `odoo-orchestrator-runtime-architecture.md` describes (raw `pg_dump`,
/// filestore not included; see the module doc comment for the "zip" format
/// this deliberately doesn't implement).
pub async fn dump_database(conn: &PgConnInfo, name: &str, out_file: &Path) -> Result<(), PgAdminError> {
    let conn = conn.clone();
    let name = name.to_string();
    let out_file = out_file.to_path_buf();
    spawn_blocking(move || {
        let mut cmd = Command::new(conn.bin_dir.join("pg_dump"));
        conn.apply_common_args(&mut cmd);
        cmd.arg("--file").arg(&out_file).arg(&name);
        let output = run(cmd, "pg_dump")?;
        check_success(&output, "pg_dump")
    })
    .await
    .expect("pg_dump blocking task panicked")
}

/// Restores a plain-SQL dump (from `dump_database`) into `name`, which must
/// not already exist — it's created fresh, then the dump is replayed
/// through `psql`.
pub async fn restore_database(conn: &PgConnInfo, name: &str, dump_file: &Path) -> Result<(), PgAdminError> {
    create_database(conn, name).await?;
    let conn = conn.clone();
    let name = name.to_string();
    let dump_file = dump_file.to_path_buf();
    spawn_blocking(move || {
        let file = std::fs::File::open(&dump_file).map_err(|e| PgAdminError::Spawn { command: "open dump file".into(), source: e })?;
        let mut cmd = Command::new(conn.bin_dir.join("psql"));
        conn.apply_common_args(&mut cmd);
        cmd.arg(&name).stdin(Stdio::from(file));
        let output = run(cmd, "psql (restore)")?;
        check_success(&output, "psql (restore)")
    })
    .await
    .expect("restore_database blocking task panicked")
}

fn escape_sql_literal(s: &str) -> String {
    s.replace('\'', "''")
}

fn escape_identifier(s: &str) -> String {
    s.replace('"', "\"\"")
}

fn run(mut cmd: Command, name: &str) -> Result<Output, PgAdminError> {
    cmd.output().map_err(|e| PgAdminError::Spawn { command: name.to_string(), source: e })
}

fn check_success(output: &Output, name: &str) -> Result<(), PgAdminError> {
    if output.status.success() {
        Ok(())
    } else {
        Err(PgAdminError::CommandFailed {
            command: name.to_string(),
            exit_code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::postgres::{discover_system_bin_dir, sandbox_root_workaround_user, PgConfig, PgSupervisor};
    use tempfile::TempDir;

    /// Starts a real, throwaway Postgres cluster for one test and returns
    /// (the supervisor, so the caller can stop it, and) connection info for
    /// `pg_admin`'s own functions to use.
    async fn start_test_cluster(dir: &std::path::Path, port: u16) -> (PgSupervisor, PgConnInfo) {
        let bin_dir = discover_system_bin_dir().expect("these tests need a system Postgres installed");
        let mut config = PgConfig::new(bin_dir.clone(), dir, port);
        if let Some((uid, gid)) = sandbox_root_workaround_user() {
            std::os::unix::fs::chown(dir, Some(uid), Some(gid)).expect("chown temp data dir for pg_admin test");
            config.run_as = Some((uid, gid));
        }
        let sup = PgSupervisor::new(config);
        sup.start().await.expect("test cluster should start cleanly");
        let conn = PgConnInfo::new(bin_dir, dir.to_path_buf(), port);
        (sup, conn)
    }

    /// Runs `psql -c <sql>` against `database` — a tiny helper so tests can
    /// insert/read real rows without pg_admin needing a general "run
    /// arbitrary SQL" API of its own (deliberately not exposed — every
    /// public function here is a specific, named lifecycle operation).
    fn psql_exec(conn: &PgConnInfo, database: &str, sql: &str) -> String {
        let mut cmd = Command::new(conn.bin_dir.join("psql"));
        conn.apply_common_args(&mut cmd);
        cmd.arg(database).arg("-tAc").arg(sql);
        let output = cmd.output().expect("psql should run");
        assert!(output.status.success(), "psql failed: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    #[tokio::test]
    async fn create_exists_and_drop_a_real_database() {
        let dir = TempDir::new().unwrap();
        let (sup, conn) = start_test_cluster(dir.path(), 55450).await;

        assert!(!database_exists(&conn, "acme").await.unwrap());
        create_database(&conn, "acme").await.unwrap();
        assert!(database_exists(&conn, "acme").await.unwrap());

        drop_database(&conn, "acme").await.unwrap();
        assert!(!database_exists(&conn, "acme").await.unwrap());

        // Idempotent: dropping an already-absent database is not an error.
        drop_database(&conn, "acme").await.unwrap();

        sup.stop().await.unwrap();
    }

    #[tokio::test]
    async fn query_scalar_returns_a_real_value_and_none_for_no_rows() {
        let dir = TempDir::new().unwrap();
        let (sup, conn) = start_test_cluster(dir.path(), 55461).await;
        create_database(&conn, "acme").await.unwrap();
        psql_exec(&conn, "acme", "CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('a real value')");

        let value = query_scalar(&conn, "acme", "SELECT v FROM t").await.unwrap();
        assert_eq!(value, Some("a real value".to_string()));

        let none = query_scalar(&conn, "acme", "SELECT v FROM t WHERE v = 'nonexistent'").await.unwrap();
        assert_eq!(none, None);

        sup.stop().await.unwrap();
    }

    #[tokio::test]
    async fn query_rows_returns_every_real_row_and_column_in_order() {
        let dir = TempDir::new().unwrap();
        let (sup, conn) = start_test_cluster(dir.path(), 55463).await;
        create_database(&conn, "acme").await.unwrap();
        psql_exec(
            &conn,
            "acme",
            "CREATE TABLE t (name TEXT, note TEXT); \
             INSERT INTO t VALUES ('a', 'first'), ('b', 'second'), ('c', NULL)",
        );

        let rows = query_rows(&conn, "acme", "SELECT name, note FROM t ORDER BY name").await.unwrap();
        assert_eq!(rows, vec![
            vec!["a".to_string(), "first".to_string()],
            vec!["b".to_string(), "second".to_string()],
            vec!["c".to_string(), "".to_string()],
        ]);

        let empty = query_rows(&conn, "acme", "SELECT name FROM t WHERE name = 'nonexistent'").await.unwrap();
        assert!(empty.is_empty());

        sup.stop().await.unwrap();
    }

    #[tokio::test]
    async fn ensure_role_creates_a_real_non_superuser_role_and_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let (sup, conn) = start_test_cluster(dir.path(), 55460).await;

        let is_role = |name: &str| psql_exec(&conn, "postgres", &format!("SELECT 1 FROM pg_roles WHERE rolname = '{name}'"));
        assert_ne!(is_role("odoo"), "1", "sanity check: role must not exist yet");

        ensure_role(&conn, "odoo").await.expect("should create a real role");
        assert_eq!(is_role("odoo"), "1", "the role must actually exist in Postgres now");
        let is_superuser = psql_exec(&conn, "postgres", "SELECT rolsuper::text FROM pg_roles WHERE rolname = 'odoo'");
        assert_eq!(is_superuser, "false", "the whole point is a non-superuser role, unlike this project's own 'postgres' admin role");

        // Idempotent: calling again when the role already exists must not error.
        ensure_role(&conn, "odoo").await.expect("calling again on an existing role should be a no-op, not an error");

        sup.stop().await.unwrap();
    }

    /// The real Postgres-15+ landmine `grant_schema_privileges` exists for:
    /// a fresh non-superuser role can log in but can't create a table in a
    /// fresh database's `public` schema until explicitly granted — this
    /// proves both halves for real (rejected before, allowed after), not
    /// just that the `GRANT` command itself didn't error.
    #[tokio::test]
    async fn grant_schema_privileges_lets_a_non_superuser_role_create_tables() {
        let dir = TempDir::new().unwrap();
        let (sup, conn) = start_test_cluster(dir.path(), 55462).await;
        create_database(&conn, "acme").await.unwrap();
        ensure_role(&conn, "odoo").await.unwrap();
        let odoo_conn = PgConnInfo { user: "odoo".to_string(), ..conn.clone() };

        let mut cmd = Command::new(conn.bin_dir.join("psql"));
        odoo_conn.apply_common_args(&mut cmd);
        cmd.arg("acme").arg("-c").arg("CREATE TABLE t (v TEXT)");
        let before = cmd.output().unwrap();
        assert!(!before.status.success(), "sanity check: must be rejected before the grant");
        assert!(String::from_utf8_lossy(&before.stderr).contains("permission denied"));

        grant_schema_privileges(&conn, "acme", "odoo").await.expect("should grant real schema privileges");

        let mut cmd = Command::new(conn.bin_dir.join("psql"));
        odoo_conn.apply_common_args(&mut cmd);
        cmd.arg("acme").arg("-c").arg("CREATE TABLE t (v TEXT)");
        let after = cmd.output().unwrap();
        assert!(after.status.success(), "should succeed after the grant: {}", String::from_utf8_lossy(&after.stderr));

        // Idempotent: granting again must not error.
        grant_schema_privileges(&conn, "acme", "odoo").await.expect("granting again should be a no-op, not an error");

        sup.stop().await.unwrap();
    }

    #[tokio::test]
    async fn duplicate_database_carries_over_real_data() {
        let dir = TempDir::new().unwrap();
        let (sup, conn) = start_test_cluster(dir.path(), 55451).await;

        create_database(&conn, "acme").await.unwrap();
        psql_exec(&conn, "acme", "CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('hello from acme')");

        duplicate_database(&conn, "acme", "acme_copy").await.unwrap();

        let value = psql_exec(&conn, "acme_copy", "SELECT v FROM t");
        assert_eq!(value, "hello from acme", "duplicated database should have the source's real data, not an empty template");

        // The source should be untouched and still usable after being used
        // as a template (proves force-disconnect didn't leave it broken).
        let source_value = psql_exec(&conn, "acme", "SELECT v FROM t");
        assert_eq!(source_value, "hello from acme");

        sup.stop().await.unwrap();
    }

    #[tokio::test]
    async fn dump_and_restore_round_trips_real_data() {
        let dir = TempDir::new().unwrap();
        let (sup, conn) = start_test_cluster(dir.path(), 55452).await;
        let dump_path = dir.path().join("acme.sql");

        create_database(&conn, "acme").await.unwrap();
        psql_exec(&conn, "acme", "CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('round trip me')");

        dump_database(&conn, "acme", &dump_path).await.unwrap();
        assert!(dump_path.is_file(), "pg_dump should have produced a real file");
        let dump_contents = std::fs::read_to_string(&dump_path).unwrap();
        assert!(dump_contents.contains("round trip me"), "the dump file should contain the actual row data");

        drop_database(&conn, "acme").await.unwrap();

        restore_database(&conn, "acme_restored", &dump_path).await.unwrap();
        let value = psql_exec(&conn, "acme_restored", "SELECT v FROM t");
        assert_eq!(value, "round trip me", "restoring the dump should recreate the original data in a fresh database");

        sup.stop().await.unwrap();
    }
}
