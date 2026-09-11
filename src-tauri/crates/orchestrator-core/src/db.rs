//! SQLite state: servers, databases, addons sources, and the append-only
//! event log. This is the source of truth `Core` mutates through — the event
//! log is a side effect of writing here, not an independent event-sourcing
//! system (deliberately lighter-weight; see the technical-design doc).

use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use uuid::Uuid;

use crate::events::EventEnvelope;
use std::collections::HashMap;

use crate::model::{AddonsSource, Database, DatabaseOrigin, OdooServer, PostgresInstance, Project, ReloadPolicy, ServerState, Snapshot, SourceKind};
use crate::CoreError;

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    /// Open (creating if needed) and migrate the SQLite database at `path`.
    /// Pass `:memory:` for tests.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let conn = Connection::open(path)?;
        let db = Self { conn: Mutex::new(conn) };
        db.init_schema()?;
        Ok(db)
    }

    fn init_schema(&self) -> Result<(), CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS projects (
                id           TEXT PRIMARY KEY,
                name         TEXT NOT NULL,
                color        INTEGER NOT NULL DEFAULT 0,
                created_at   TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS servers (
                id                   TEXT PRIMARY KEY,
                project_id           TEXT REFERENCES projects(id),
                name                 TEXT NOT NULL,
                odoo_version         TEXT NOT NULL,
                port                 INTEGER NOT NULL,
                postgres_instance_id TEXT NOT NULL REFERENCES postgres_instances(id),
                state                TEXT NOT NULL,
                exit_code            INTEGER,
                started_at           TEXT,
                created_at           TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS addons_sources (
                id            TEXT PRIMARY KEY,
                server_id     TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
                label         TEXT NOT NULL,
                path_or_url   TEXT NOT NULL,
                kind          TEXT NOT NULL,
                rank          INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS databases (
                id             TEXT PRIMARY KEY,
                server_id      TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
                name           TEXT NOT NULL,
                domain         TEXT,
                size_bytes     INTEGER NOT NULL DEFAULT 0,
                last_backup_at TEXT,
                created_at     TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS postgres_instances (
                id           TEXT PRIMARY KEY,
                label        TEXT NOT NULL,
                pg_version   TEXT NOT NULL,
                port         INTEGER NOT NULL,
                data_dir     TEXT NOT NULL,
                created_at   TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS snapshots (
                id                      TEXT PRIMARY KEY,
                server_id               TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
                database_id             TEXT NOT NULL,
                source_database_name    TEXT NOT NULL,
                snapshot_database_name  TEXT NOT NULL,
                name                    TEXT NOT NULL,
                filestore_snapshot_path TEXT,
                created_at              TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS events (
                id           TEXT PRIMARY KEY,
                kind         TEXT NOT NULL,
                occurred_at  TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                duration_ms  INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_events_occurred_at ON events(occurred_at);

            -- What each module's files hashed to the last time it was
            -- installed or updated *on this database*. This is what turns
            -- "is my code actually live?" from a guess into an answer:
            -- comparing the manifest's declared version only notices a
            -- change when someone remembered to bump it, and nobody bumps
            -- it while iterating.
            CREATE TABLE IF NOT EXISTS module_installs (
                database_id    TEXT NOT NULL,
                technical_name TEXT NOT NULL,
                content_hash   TEXT NOT NULL,
                updated_at     TEXT NOT NULL,
                PRIMARY KEY (database_id, technical_name)
            );
            "#,
        )?;
        // Migration for a `servers` table created before `started_at` existed
        // — `CREATE TABLE IF NOT EXISTS` above doesn't add columns to an
        // already-existing table. Expected to fail with "duplicate column"
        // on every database that already has it (including every freshly
        // created one, since the CREATE TABLE already includes it), so the
        // result is intentionally discarded.
        let _ = conn.execute("ALTER TABLE servers ADD COLUMN started_at TEXT", []);
        // Same story for `project_id`, added when projects became the top
        // level of the UI. Rows written before that keep NULL here until
        // `Core::open`'s adoption pass files them under a default project.
        let _ = conn.execute("ALTER TABLE servers ADD COLUMN project_id TEXT REFERENCES projects(id)", []);
        // Same story again for a database's own hostname.
        let _ = conn.execute("ALTER TABLE databases ADD COLUMN domain TEXT", []);
        let _ = conn.execute("ALTER TABLE events ADD COLUMN duration_ms INTEGER", []);
        // Which branch a snapshot was taken on (task 3.2). Idempotent:
        // fails harmlessly on a database that already has it.
        let _ = conn.execute("ALTER TABLE snapshots ADD COLUMN git_branch TEXT", []);
        // What an Odoo does when code on disk changes. Both default to
        // off: restarting somebody's Odoo unasked is exactly the kind of
        // surprise this project's design rules exist to prevent.
        let _ = conn.execute("ALTER TABLE servers ADD COLUMN reload_python INTEGER NOT NULL DEFAULT 0", []);
        let _ = conn.execute("ALTER TABLE servers ADD COLUMN update_on_data_change INTEGER NOT NULL DEFAULT 0", []);
        // Rows written before origins existed default to "created", which
        // is the *least* alarming answer — deliberately, because guessing
        // "restored" for a database nobody restored would cry wolf on every
        // existing row and teach people to ignore the warning.
        let _ = conn.execute("ALTER TABLE databases ADD COLUMN origin TEXT NOT NULL DEFAULT 'created'", []);
        Ok(())
    }

    pub fn insert_project(&self, project: &Project) -> Result<(), CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "INSERT INTO projects (id, name, color, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                project.id.to_string(),
                project.name,
                project.color,
                project.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn list_projects(&self) -> Result<Vec<Project>, CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt = conn.prepare("SELECT id, name, color, created_at FROM projects ORDER BY created_at")?;
        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let created_at: String = row.get(3)?;
            Ok(Project {
                id: Uuid::parse_str(&id).unwrap_or_default(),
                name: row.get(1)?,
                color: row.get::<_, i64>(2)? as u8,
                created_at: parse_dt(&created_at),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(CoreError::from)
    }

    pub fn get_project(&self, project_id: Uuid) -> Result<Option<Project>, CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt = conn.prepare("SELECT id, name, color, created_at FROM projects WHERE id = ?1")?;
        let mut rows = stmt.query_map(params![project_id.to_string()], |row| {
            let id: String = row.get(0)?;
            let created_at: String = row.get(3)?;
            Ok(Project {
                id: Uuid::parse_str(&id).unwrap_or_default(),
                name: row.get(1)?,
                color: row.get::<_, i64>(2)? as u8,
                created_at: parse_dt(&created_at),
            })
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    pub fn update_project(&self, project_id: Uuid, name: &str, color: u8) -> Result<(), CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "UPDATE projects SET name = ?2, color = ?3 WHERE id = ?1",
            params![project_id.to_string(), name, color],
        )?;
        Ok(())
    }

    pub fn delete_project(&self, project_id: Uuid) -> Result<(), CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute("DELETE FROM projects WHERE id = ?1", params![project_id.to_string()])?;
        Ok(())
    }

    /// How many Odoos are filed under a project. Used to refuse deleting a
    /// project that still holds work rather than cascading into someone's
    /// databases — see `Core::delete_project`.
    pub fn count_servers_in_project(&self, project_id: Uuid) -> Result<u32, CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM servers WHERE project_id = ?1",
            params![project_id.to_string()],
            |row| row.get(0),
        )?;
        Ok(n as u32)
    }

    /// Files every Odoo that has no project yet under `project_id`. Returns
    /// how many were adopted, so startup can skip recording an event when
    /// there was nothing to migrate.
    pub fn adopt_orphan_servers(&self, project_id: Uuid) -> Result<u32, CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let n = conn.execute(
            "UPDATE servers SET project_id = ?1 WHERE project_id IS NULL",
            params![project_id.to_string()],
        )?;
        Ok(n as u32)
    }

    pub fn count_orphan_servers(&self) -> Result<u32, CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM servers WHERE project_id IS NULL", [], |row| row.get(0))?;
        Ok(n as u32)
    }

    /// Every port already spoken for by an Odoo, so a new one (or a
    /// duplicated project's copies) can be given a free one instead of
    /// colliding on startup.
    pub fn used_ports(&self) -> Result<Vec<u16>, CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt = conn.prepare("SELECT port FROM servers")?;
        let rows = stmt.query_map([], |row| Ok(row.get::<_, i64>(0)? as u16))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(CoreError::from)
    }

    /// Persists what this Odoo should do when code on disk changes.
    pub fn set_reload_policy(&self, server_id: Uuid, policy: ReloadPolicy) -> Result<(), CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "UPDATE servers SET reload_python = ?1, update_on_data_change = ?2 WHERE id = ?3",
            params![policy.reload_python as i64, policy.update_on_data_change as i64, server_id.to_string()],
        )?;
        Ok(())
    }

    pub fn set_server_project(&self, server_id: Uuid, project_id: Uuid) -> Result<(), CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "UPDATE servers SET project_id = ?2 WHERE id = ?1",
            params![server_id.to_string(), project_id.to_string()],
        )?;
        Ok(())
    }

    pub fn insert_server(&self, server: &OdooServer) -> Result<(), CoreError> {
        let (state_str, exit_code) = encode_state(&server.state);
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "INSERT INTO servers (id, project_id, name, odoo_version, port, postgres_instance_id, state, exit_code, started_at, created_at)
             VALUES (?1, ?10, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                server.id.to_string(),
                server.name,
                server.odoo_version,
                server.port,
                server.postgres_instance_id.to_string(),
                state_str,
                exit_code,
                server.started_at.map(|t| t.to_rfc3339()),
                server.created_at.to_rfc3339(),
                server.project_id.map(|id| id.to_string()),
            ],
        )?;
        Ok(())
    }

    pub fn list_servers(&self) -> Result<Vec<OdooServer>, CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, name, odoo_version, port, postgres_instance_id, state, exit_code, started_at, created_at, project_id, reload_python, update_on_data_change FROM servers ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let postgres_instance_id: String = row.get(4)?;
            let state_str: String = row.get(5)?;
            let exit_code: Option<i32> = row.get(6)?;
            let started_at: Option<String> = row.get(7)?;
            let created_at: String = row.get(8)?;
            let project_id: Option<String> = row.get(9)?;
            Ok(OdooServer {
                id: Uuid::parse_str(&id).unwrap_or_default(),
                project_id: project_id.as_deref().and_then(|p| Uuid::parse_str(p).ok()),
                name: row.get(1)?,
                odoo_version: row.get(2)?,
                port: row.get::<_, i64>(3)? as u16,
                postgres_instance_id: Uuid::parse_str(&postgres_instance_id).unwrap_or_default(),
                state: decode_state(&state_str, exit_code),
                reload: ReloadPolicy {
                    reload_python: row.get::<_, i64>(10)? != 0,
                    update_on_data_change: row.get::<_, i64>(11)? != 0,
                },
                started_at: started_at.as_deref().map(parse_dt),
                created_at: parse_dt(&created_at),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(CoreError::from)
    }

    pub fn get_server(&self, server_id: Uuid) -> Result<Option<OdooServer>, CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, name, odoo_version, port, postgres_instance_id, state, exit_code, started_at, created_at, project_id, reload_python, update_on_data_change FROM servers WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![server_id.to_string()], |row| {
            let id: String = row.get(0)?;
            let postgres_instance_id: String = row.get(4)?;
            let state_str: String = row.get(5)?;
            let exit_code: Option<i32> = row.get(6)?;
            let started_at: Option<String> = row.get(7)?;
            let created_at: String = row.get(8)?;
            let project_id: Option<String> = row.get(9)?;
            Ok(OdooServer {
                id: Uuid::parse_str(&id).unwrap_or_default(),
                project_id: project_id.as_deref().and_then(|p| Uuid::parse_str(p).ok()),
                name: row.get(1)?,
                odoo_version: row.get(2)?,
                port: row.get::<_, i64>(3)? as u16,
                postgres_instance_id: Uuid::parse_str(&postgres_instance_id).unwrap_or_default(),
                state: decode_state(&state_str, exit_code),
                reload: ReloadPolicy {
                    reload_python: row.get::<_, i64>(10)? != 0,
                    update_on_data_change: row.get::<_, i64>(11)? != 0,
                },
                started_at: started_at.as_deref().map(parse_dt),
                created_at: parse_dt(&created_at),
            })
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Persists a live `OdooSupervisor` state transition to the server's own
    /// row — unlike `PostgresInstance` (state kept purely in-memory in
    /// `Core::postgres_supervisors`), `OdooServer::state` is a real column,
    /// so `get_server`/`list_servers` reflect it without the caller needing
    /// a separate live-state query.
    pub fn update_server_state(&self, server_id: Uuid, state: &ServerState) -> Result<(), CoreError> {
        let (state_str, exit_code) = encode_state(state);
        let conn = self.conn.lock().expect("db mutex poisoned");
        // `started_at` tracks the most recent `Running` transition — cleared
        // on `Stopped`/`Crashed` so a stopped server never reports stale
        // uptime, left untouched on `Starting`/`Stopping` since those aren't
        // "running" yet/anymore.
        match state {
            ServerState::Running => conn.execute(
                "UPDATE servers SET state = ?1, exit_code = ?2, started_at = ?3 WHERE id = ?4",
                params![state_str, exit_code, Utc::now().to_rfc3339(), server_id.to_string()],
            )?,
            ServerState::Stopped | ServerState::Crashed { .. } => conn.execute(
                "UPDATE servers SET state = ?1, exit_code = ?2, started_at = NULL WHERE id = ?3",
                params![state_str, exit_code, server_id.to_string()],
            )?,
            ServerState::Starting | ServerState::Stopping => conn.execute(
                "UPDATE servers SET state = ?1, exit_code = ?2 WHERE id = ?3",
                params![state_str, exit_code, server_id.to_string()],
            )?,
        };
        Ok(())
    }

    pub fn insert_addons_source(&self, source: &AddonsSource) -> Result<(), CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "INSERT INTO addons_sources (id, server_id, label, path_or_url, kind, rank)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                source.id.to_string(),
                source.server_id.to_string(),
                source.label,
                source.path_or_url,
                kind_str(source.kind),
                source.rank,
            ],
        )?;
        Ok(())
    }

    pub fn list_addons_sources(&self, server_id: Uuid) -> Result<Vec<AddonsSource>, CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, server_id, label, path_or_url, kind, rank FROM addons_sources WHERE server_id = ?1 ORDER BY rank",
        )?;
        let rows = stmt.query_map(params![server_id.to_string()], |row| {
            let id: String = row.get(0)?;
            let server_id: String = row.get(1)?;
            let kind_str: String = row.get(4)?;
            Ok(AddonsSource {
                id: Uuid::parse_str(&id).unwrap_or_default(),
                server_id: Uuid::parse_str(&server_id).unwrap_or_default(),
                label: row.get(2)?,
                path_or_url: row.get(3)?,
                kind: decode_kind(&kind_str),
                rank: row.get::<_, i64>(5)? as u32,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(CoreError::from)
    }

    pub fn set_database_domain(&self, database_id: Uuid, domain: Option<&str>) -> Result<(), CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "UPDATE databases SET domain = ?2 WHERE id = ?1",
            params![database_id.to_string(), domain],
        )?;
        Ok(())
    }

    pub fn insert_database(&self, database: &Database) -> Result<(), CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "INSERT INTO databases (id, server_id, name, size_bytes, last_backup_at, created_at, domain, origin)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                database.id.to_string(),
                database.server_id.to_string(),
                database.name,
                database.size_bytes,
                database.last_backup_at.map(|d| d.to_rfc3339()),
                database.created_at.to_rfc3339(),
                database.domain,
                encode_origin(database.origin),
            ],
        )?;
        Ok(())
    }

    pub fn list_databases(&self, server_id: Option<Uuid>) -> Result<Vec<Database>, CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let (sql, filter) = match server_id {
            Some(id) => (
                "SELECT id, server_id, name, size_bytes, last_backup_at, created_at, domain, origin FROM databases WHERE server_id = ?1 ORDER BY created_at",
                Some(id.to_string()),
            ),
            None => (
                "SELECT id, server_id, name, size_bytes, last_backup_at, created_at, domain, origin FROM databases ORDER BY created_at",
                None,
            ),
        };
        let mut stmt = conn.prepare(sql)?;
        let map_row = |row: &rusqlite::Row| -> rusqlite::Result<Database> {
            let id: String = row.get(0)?;
            let server_id: String = row.get(1)?;
            let last_backup_at: Option<String> = row.get(4)?;
            let created_at: String = row.get(5)?;
            Ok(Database {
                id: Uuid::parse_str(&id).unwrap_or_default(),
                server_id: Uuid::parse_str(&server_id).unwrap_or_default(),
                name: row.get(2)?,
                domain: row.get(6)?,
                origin: decode_origin(&row.get::<_, String>(7)?),
                size_bytes: row.get::<_, i64>(3)? as u64,
                last_backup_at: last_backup_at.map(|d| parse_dt(&d)),
                created_at: parse_dt(&created_at),
            })
        };
        let rows = match filter {
            Some(id) => stmt.query_map(params![id], map_row)?.collect::<Result<Vec<_>, _>>(),
            None => stmt.query_map([], map_row)?.collect::<Result<Vec<_>, _>>(),
        };
        rows.map_err(CoreError::from)
    }

    pub fn delete_database(&self, database_id: Uuid) -> Result<(), CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute("DELETE FROM databases WHERE id = ?1", params![database_id.to_string()])?;
        Ok(())
    }

    /// Writes a size measured from the real cluster onto the metadata row.
    /// Sizes are refreshed on demand rather than kept live: `pg_database_size`
    /// is a real query against a real server, and running one per database
    /// on every list would make the Databases screen slower the more work
    /// someone has.
    pub fn set_database_size(&self, database_id: Uuid, size_bytes: u64) -> Result<(), CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "UPDATE databases SET size_bytes = ?1 WHERE id = ?2",
            params![size_bytes as i64, database_id.to_string()],
        )?;
        Ok(())
    }

    pub fn mark_database_backed_up(&self, database_id: Uuid, at: DateTime<Utc>) -> Result<(), CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "UPDATE databases SET last_backup_at = ?1 WHERE id = ?2",
            params![at.to_rfc3339(), database_id.to_string()],
        )?;
        Ok(())
    }

    pub fn insert_postgres_instance(&self, instance: &PostgresInstance) -> Result<(), CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "INSERT INTO postgres_instances (id, label, pg_version, port, data_dir, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                instance.id.to_string(),
                instance.label,
                instance.pg_version,
                instance.port,
                instance.data_dir,
                instance.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn list_postgres_instances(&self) -> Result<Vec<PostgresInstance>, CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, label, pg_version, port, data_dir, created_at FROM postgres_instances ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let created_at: String = row.get(5)?;
            Ok(PostgresInstance {
                id: Uuid::parse_str(&id).unwrap_or_default(),
                label: row.get(1)?,
                pg_version: row.get(2)?,
                port: row.get::<_, i64>(3)? as u16,
                data_dir: row.get(4)?,
                created_at: parse_dt(&created_at),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(CoreError::from)
    }

    pub fn get_postgres_instance(&self, id: Uuid) -> Result<Option<PostgresInstance>, CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, label, pg_version, port, data_dir, created_at FROM postgres_instances WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id.to_string()], |row| {
            let row_id: String = row.get(0)?;
            let created_at: String = row.get(5)?;
            Ok(PostgresInstance {
                id: Uuid::parse_str(&row_id).unwrap_or_default(),
                label: row.get(1)?,
                pg_version: row.get(2)?,
                port: row.get::<_, i64>(3)? as u16,
                data_dir: row.get(4)?,
                created_at: parse_dt(&created_at),
            })
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    pub fn insert_snapshot(&self, snapshot: &Snapshot) -> Result<(), CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "INSERT INTO snapshots (id, server_id, database_id, source_database_name, snapshot_database_name, name, filestore_snapshot_path, git_branch, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                snapshot.id.to_string(),
                snapshot.server_id.to_string(),
                snapshot.database_id.to_string(),
                snapshot.source_database_name,
                snapshot.snapshot_database_name,
                snapshot.name,
                snapshot.filestore_snapshot_path,
                snapshot.git_branch,
                snapshot.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn list_snapshots(&self, database_id: Option<Uuid>) -> Result<Vec<Snapshot>, CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let (sql, filter) = match database_id {
            Some(id) => (
                "SELECT id, server_id, database_id, source_database_name, snapshot_database_name, name, filestore_snapshot_path, git_branch, created_at
                 FROM snapshots WHERE database_id = ?1 ORDER BY created_at",
                Some(id.to_string()),
            ),
            None => (
                "SELECT id, server_id, database_id, source_database_name, snapshot_database_name, name, filestore_snapshot_path, git_branch, created_at
                 FROM snapshots ORDER BY created_at",
                None,
            ),
        };
        let mut stmt = conn.prepare(sql)?;
        let map_row = map_snapshot_row;
        let rows = match filter {
            Some(id) => stmt.query_map(params![id], map_row)?.collect::<Result<Vec<_>, _>>(),
            None => stmt.query_map([], map_row)?.collect::<Result<Vec<_>, _>>(),
        };
        rows.map_err(CoreError::from)
    }

    pub fn get_snapshot(&self, id: Uuid) -> Result<Option<Snapshot>, CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, server_id, database_id, source_database_name, snapshot_database_name, name, filestore_snapshot_path, git_branch, created_at
             FROM snapshots WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id.to_string()], map_snapshot_row)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    pub fn delete_snapshot(&self, id: Uuid) -> Result<(), CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute("DELETE FROM snapshots WHERE id = ?1", params![id.to_string()])?;
        Ok(())
    }

    pub fn append_event(&self, envelope: &EventEnvelope) -> Result<(), CoreError> {
        let payload_json = serde_json::to_string(&envelope.event)?;
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "INSERT INTO events (id, kind, occurred_at, payload_json, duration_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                envelope.id.to_string(),
                envelope.event.kind(),
                envelope.occurred_at.to_rfc3339(),
                payload_json,
                envelope.duration_ms,
            ],
        )?;
        Ok(())
    }

    pub fn recent_events(&self, limit: u32) -> Result<Vec<EventEnvelope>, CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, occurred_at, payload_json, duration_ms FROM events ORDER BY occurred_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            let id: String = row.get(0)?;
            let occurred_at: String = row.get(1)?;
            let payload_json: String = row.get(2)?;
            let duration_ms: Option<i64> = row.get(3)?;
            Ok((id, occurred_at, payload_json, duration_ms))
        })?;

        let mut out = Vec::new();
        for row in rows {
            let (id, occurred_at, payload_json, duration_ms) = row?;
            let event = serde_json::from_str(&payload_json)?;
            out.push(EventEnvelope {
                id: Uuid::parse_str(&id).unwrap_or_default(),
                occurred_at: parse_dt(&occurred_at),
                duration_ms: duration_ms.map(|d| d as u64),
                event,
            });
        }
        Ok(out)
    }
}

fn map_snapshot_row(row: &rusqlite::Row) -> rusqlite::Result<Snapshot> {
    let id: String = row.get(0)?;
    let server_id: String = row.get(1)?;
    let database_id: String = row.get(2)?;
    let created_at: String = row.get(8)?;
    Ok(Snapshot {
        id: Uuid::parse_str(&id).unwrap_or_default(),
        server_id: Uuid::parse_str(&server_id).unwrap_or_default(),
        database_id: Uuid::parse_str(&database_id).unwrap_or_default(),
        source_database_name: row.get(3)?,
        snapshot_database_name: row.get(4)?,
        name: row.get(5)?,
        filestore_snapshot_path: row.get(6)?,
        git_branch: row.get(7)?,
        created_at: parse_dt(&created_at),
    })
}

fn kind_str(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Private => "private",
        SourceKind::Oca => "oca",
        SourceKind::Core => "core",
    }
}

fn decode_kind(kind_str: &str) -> SourceKind {
    match kind_str {
        "oca" => SourceKind::Oca,
        "core" => SourceKind::Core,
        _ => SourceKind::Private,
    }
}

fn encode_state(state: &ServerState) -> (&'static str, Option<i32>) {
    match state {
        ServerState::Stopped => ("stopped", None),
        ServerState::Starting => ("starting", None),
        ServerState::Running => ("running", None),
        ServerState::Stopping => ("stopping", None),
        ServerState::Crashed { exit_code } => ("crashed", Some(*exit_code)),
    }
}

fn decode_state(state_str: &str, exit_code: Option<i32>) -> ServerState {
    match state_str {
        "starting" => ServerState::Starting,
        "running" => ServerState::Running,
        "stopping" => ServerState::Stopping,
        "crashed" => ServerState::Crashed { exit_code: exit_code.unwrap_or(-1) },
        _ => ServerState::Stopped,
    }
}

impl Db {
    /// Records what a module's files hashed to at the moment it was
    /// installed or updated on one database.
    pub fn record_module_install(&self, database_id: Uuid, technical_name: &str, content_hash: &str) -> Result<(), CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "INSERT INTO module_installs (database_id, technical_name, content_hash, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(database_id, technical_name) DO UPDATE SET content_hash = ?3, updated_at = ?4",
            params![database_id.to_string(), technical_name, content_hash, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    /// What each module hashed to when it was last put on this database.
    pub fn module_installs(&self, database_id: Uuid) -> Result<HashMap<String, String>, CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt = conn.prepare("SELECT technical_name, content_hash FROM module_installs WHERE database_id = ?1")?;
        let rows = stmt
            .query_map(params![database_id.to_string()], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
            .collect::<Result<HashMap<_, _>, _>>()?;
        Ok(rows)
    }

    /// Forgets a dropped database's records, so they can't be mistaken for
    /// a later database that happens to reuse the id.
    pub fn forget_module_installs(&self, database_id: Uuid) -> Result<(), CoreError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute("DELETE FROM module_installs WHERE database_id = ?1", params![database_id.to_string()])?;
        Ok(())
    }
}

fn encode_origin(origin: DatabaseOrigin) -> &'static str {
    match origin {
        DatabaseOrigin::Created => "created",
        DatabaseOrigin::Duplicated => "duplicated",
        DatabaseOrigin::Restored => "restored",
        DatabaseOrigin::Adopted => "adopted",
    }
}

/// An unrecognised value reads as `Adopted` rather than `Created`: an
/// origin this build doesn't understand is one it can't vouch for, and the
/// safe reading of "I don't know where this came from" is "assume it came
/// from somewhere else".
fn decode_origin(value: &str) -> DatabaseOrigin {
    match value {
        "created" => DatabaseOrigin::Created,
        "duplicated" => DatabaseOrigin::Duplicated,
        "restored" => DatabaseOrigin::Restored,
        _ => DatabaseOrigin::Adopted,
    }
}

fn parse_dt(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}
