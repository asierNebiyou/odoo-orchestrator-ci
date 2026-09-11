//! `orchestrator-core`: domain model, SQLite state, and the event bus for
//! Odoo Orchestrator. Deliberately has no dependency on `tauri` or any UI
//! framework — per the technical-design doc, the core is a plain library any
//! shell (Tauri today, a hosted control plane later) can embed.

pub mod bus;
pub mod db;
pub mod events;
pub mod filestore;
pub mod git;
pub mod manifest;
pub mod model;
pub mod modules;
pub mod odoo;
pub mod odoo_conf;
pub mod openupgrade;
mod odoo_runtime;
pub mod pg_admin;
pub mod postgres;
pub mod python_runtime;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use uuid::Uuid;

pub use bus::{EventBus, LogBus, LogLine};
pub use db::Db;
pub use events::{Event, EventEnvelope};
pub use filestore::{FilestoreError, SnapshotStats};
pub use git::GitError;
pub use model::{AddonsSource, Database, OdooServer, PostgresInstance, Project, ReloadPolicy, ServerState, Snapshot, SourceKind};
pub use modules::ModuleScan;
pub use odoo::{LogStream, OdooConfig, OdooError, OdooNotification, OdooState, ProcessUsage};
pub use odoo_runtime::{CachedRuntime, OdooRuntime, OdooRuntimeError, RuntimeSource, RuntimeStore};
pub use pg_admin::{PgAdminError, PgConnInfo};
pub use postgres::{PgConfig, PgError, PgState};
pub use python_runtime::{PythonRuntimeConfig, RuntimeError as PythonRuntimeError};

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("postgres error: {0}")]
    Postgres(#[from] postgres::PgError),
    #[error("postgres admin error: {0}")]
    PgAdmin(#[from] pg_admin::PgAdminError),
    #[error("postgres instance {instance_id} is not running (state: {state:?}) — start it before creating a database on it")]
    PostgresInstanceNotRunning { instance_id: Uuid, state: postgres::PgState },
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("filestore snapshot error: {0}")]
    Filestore(#[from] filestore::FilestoreError),
    #[error("snapshot {snapshot_id} was not taken from database {database_id}, so it cannot be used to revert it")]
    SnapshotNotForDatabase { snapshot_id: Uuid, database_id: Uuid },
    #[error("snapshot {snapshot_id} has no filestore copy to revert to")]
    SnapshotHasNoFilestore { snapshot_id: Uuid },
    #[error("odoo error: {0}")]
    Odoo(#[from] odoo::OdooError),
    #[error("odoo module {action} failed for database {database}: {stderr}")]
    ModuleCommandFailed { database: String, action: String, stdout: String, stderr: String },
    #[error("project {project_id} still holds {server_count} Odoo(s) — remove them first")]
    ProjectNotEmpty { project_id: Uuid, server_count: u32 },
    #[error("odoo runtime error: {0}")]
    OdooRuntime(#[from] odoo_runtime::OdooRuntimeError),
    #[error("git error: {0}")]
    Git(#[from] git::GitError),
    #[error("couldn't build Odoo's Python environment: {0}")]
    PythonRuntime(#[from] python_runtime::RuntimeError),
    #[error(transparent)]
    OpenUpgrade(#[from] openupgrade::OpenUpgradeError),
    #[error("{0}")]
    NotConfigured(String),
    #[error("{domain} already points at the database {database}")]
    DomainTaken { domain: String, database: String },
}

/// One hostname, and where a request for it should go.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    pub host: String,
    pub port: u16,
    /// The database name to put in the forwarded Host header's first
    /// label, so Odoo's own `dbfilter = ^%d$` selects the right one.
    pub database: String,
    pub server_id: Uuid,
    pub database_id: Uuid,
    pub running: bool,
}

/// A database as the *cluster* reports it, which may or may not be one
/// this app knows about.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterDatabase {
    pub instance_id: Uuid,
    pub name: String,
    pub size_bytes: u64,
}

/// A metadata row whose database isn't in the cluster any more.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissingDatabase {
    pub database_id: Uuid,
    pub name: String,
    pub server_id: Uuid,
    pub store: String,
}

/// The difference between what this app thinks exists and what does.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reconciliation {
    /// Real databases the app doesn't know about.
    pub untracked: Vec<ClusterDatabase>,
    /// Rows whose database is gone underneath them.
    pub missing: Vec<MissingDatabase>,
    /// Stores that couldn't be asked, by label. Their databases are
    /// deliberately *not* reported as missing.
    pub unchecked: Vec<String>,
}

/// Measured disk usage, broken into the parts someone can actually act on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskUsage {
    /// Sum of the last measured size of every tracked database. Refreshed
    /// by `refresh_database_sizes`, so it can be stale — the UI should say
    /// when it was last measured rather than implying it's live.
    pub database_bytes: u64,
    pub snapshot_filestore_bytes: u64,
    pub backup_bytes: u64,
    pub module_bytes: u64,
    pub odoo_copy_bytes: u64,
    pub odoo_copies: Vec<odoo_runtime::CachedRuntime>,
}

/// Whether a "where are your modules" answer is a repository to clone
/// rather than a folder that already exists. Deliberately conservative:
/// anything that isn't clearly a URL is treated as a path, because
/// mistaking a local folder for a URL fails in a much more confusing way
/// than the reverse.
fn looks_like_git_url(value: &str) -> bool {
    let v = value.trim();
    v.starts_with("https://") || v.starts_with("http://") || v.starts_with("git@") || v.starts_with("ssh://")
}

/// A port nothing is currently listening on. There is an unavoidable race
/// between letting go of the socket and Postgres claiming it, but on a
/// desktop machine choosing its own port for its own database that window
/// is not worth a lock file.
fn free_local_port() -> Result<u16, CoreError> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

/// `LogStream` as the string the UI filters on.
fn stream_name(stream: odoo::LogStream) -> &'static str {
    match stream {
        odoo::LogStream::Stdout => "stdout",
        odoo::LogStream::Stderr => "stderr",
    }
}

/// Where `uv` is. Looked up on PATH — the shipped app bundles it beside
/// its other runtimes, and a dev machine normally has it already.
fn discover_uv_bin() -> Result<std::path::PathBuf, CoreError> {
    for dir in std::env::split_paths(&std::env::var("PATH").unwrap_or_default()) {
        let candidate = dir.join("uv");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(CoreError::NotConfigured(
        "couldn't find `uv`, which this app uses to build Odoo's Python environment".into(),
    ))
}

/// Which module a changed file belongs to: the first path segment under
/// whichever watched addons folder contains it.
///
/// Only data files count. A `.py` change is Odoo's own `--dev=reload` to
/// handle, and running `-u` for it as well would mean two restarts for one
/// save. Editor scratch files (`.~`, `.swp`, `#autosave#`) are ignored —
/// they are not edits, they are an editor thinking out loud.
fn collect_modules(
    event: &notify::Result<notify::Event>,
    roots: &[std::path::PathBuf],
    into: &mut std::collections::HashSet<String>,
) {
    let Ok(event) = event else { return };
    if !matches!(event.kind, notify::EventKind::Create(_) | notify::EventKind::Modify(_)) {
        return;
    }
    for path in &event.paths {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if name.starts_with('.') || name.starts_with('#') || name.ends_with('~') || name.ends_with(".swp") {
            continue;
        }
        let is_data = matches!(path.extension().and_then(|e| e.to_str()), Some("xml") | Some("csv"));
        if !is_data {
            continue;
        }
        for root in roots {
            if let Ok(relative) = path.strip_prefix(root) {
                if let Some(module) = relative.components().next().and_then(|c| c.as_os_str().to_str()) {
                    into.insert(module.to_string());
                }
                break;
            }
        }
    }
}

fn slugify(value: &str) -> String {
    let slug: String = value
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() { "modules".to_string() } else { slug }
}

/// The first port at or above `preferred` that no Odoo already claims.
/// Used when duplicating a project, where copying a port verbatim would
/// guarantee a collision the moment both are started.
fn next_free_port(preferred: u16, used: &[u16]) -> u16 {
    let mut port = preferred;
    while used.contains(&port) {
        port = port.saturating_add(1);
        if port == u16::MAX {
            return port;
        }
    }
    port
}

/// One real row of a database's own `ir_module_module` table — the "is this
/// actually installed, and what version" fact the Modules screen's deferred
/// currency instrument (task 1.5/2.5) needs, sourced from the database
/// itself rather than fabricated. `installed_version` is `None` for a
/// module that's never been installed on this database (its row still
/// exists — Odoo tracks every *available* module, not just installed ones
/// — but with no installed version).
///
/// **A real, worth-documenting Odoo naming trap this deliberately works
/// around**: `ir_module_module`'s Python model names its *stored* column
/// (the version that's actually installed) `latest_version` — labeled
/// "Installed Version" in its own field definition — while its
/// `installed_version` Python attribute is a `compute=`d field (labeled
/// "Latest Version") that re-walks the addons path at read time and isn't
/// a real column `SELECT` can reach at all. So this struct's
/// `installed_version` is sourced from Odoo's stored `latest_version`
/// column, renamed here to what it actually represents rather than
/// propagating Odoo's own confusing field-name/label inversion. To learn
/// what's genuinely on disk right now (Odoo's compute-only "Latest
/// Version"), combine this with `Core::scan_modules`'s own
/// `ModuleListing::version` for the same `technical_name` — this project
/// already parses that independently via its own manifest parser (see
/// `manifest.rs`), so it doesn't need Odoo's ORM for that half either.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseModuleState {
    pub technical_name: String,
    /// Odoo's own `ir_module_module.state` values, e.g. `"installed"`,
    /// `"uninstalled"`, `"to upgrade"` — passed through verbatim rather than
    /// reinterpreted, since Odoo itself is the authority on what these mean.
    pub state: String,
    pub installed_version: Option<String>,
}

/// The shared handle the rest of the app (orchestrator-api, and eventually
/// the Tauri shell) holds. Cheap to clone — wraps `Arc`s internally.
#[derive(Clone)]
pub struct Core {
    db: Arc<Db>,
    bus: EventBus,
    /// Live `PgSupervisor` handles, keyed by `PostgresInstance` id. SQLite
    /// (via `db`) holds each instance's static config (label/version/port/
    /// data_dir); this holds the *running* supervisor object, which carries
    /// in-memory state (current `PgState`, the background health-watcher
    /// task) that has no business being reconstructed from disk on every
    /// call. Empty on a fresh `Core::open` — nothing auto-starts; a
    /// previously-registered instance is just inert until something calls
    /// `start_postgres_instance` again.
    postgres_supervisors: Arc<Mutex<HashMap<Uuid, postgres::PgSupervisor>>>,
    /// Live `OdooSupervisor` handles, keyed by `OdooServer` id — same split
    /// as `postgres_supervisors` above, except `OdooServer::state` (unlike
    /// `PostgresInstance`) *is* persisted to SQLite on every transition (see
    /// `Db::update_server_state`), so `get_server`/`list_servers` reflect
    /// live state without a parallel lookup here.
    odoo_supervisors: Arc<Mutex<HashMap<Uuid, odoo::OdooSupervisor>>>,
    /// Where Odoo checkouts are cached and looked for. `None` in tests
    /// that never start a real Odoo; set by the shell at startup.
    runtime_store: Arc<Mutex<Option<odoo_runtime::RuntimeStore>>>,
    /// The app's own folder — where an auto-created database store and
    /// per-Odoo working files go, so neither has to be asked for.
    data_dir: Arc<Mutex<Option<std::path::PathBuf>>>,
    /// The filestore directory each running Odoo was actually configured
    /// with, keyed by server id. Recorded by `start_server` so a snapshot
    /// finds the real files even when a caller passed its own
    /// `runtime_dir`; empty after a restart, which is why
    /// `database_filestore_path` falls back to the standard layout.
    filestore_dirs: Arc<Mutex<HashMap<Uuid, std::path::PathBuf>>>,
    /// The last N lines of real output from each Odoo, and a channel for
    /// them. When Odoo raises a traceback, this is where it is — the
    /// supervisor has always streamed these lines and Core used to throw
    /// every one away, so the app could tell you an Odoo had crashed but
    /// not why.
    ///
    /// In memory and bounded, deliberately: process output is high-volume
    /// and only interesting while you're looking at it. Persisting it
    /// would mean an unbounded table nobody prunes, and mixing it into the
    /// event log would bury the record of what the *app* did.
    log_buffers: Arc<Mutex<HashMap<Uuid, std::collections::VecDeque<LogLine>>>>,
    log_bus: LogBus,
    /// Live filesystem watchers, one per Odoo that asked for one. Held
    /// because a `notify` watcher stops the moment its handle is dropped —
    /// so this map *is* the on/off switch.
    data_watchers: Arc<Mutex<HashMap<Uuid, notify::RecommendedWatcher>>>,
}

/// Exactly what starting one Odoo would run: the generated `odoo.conf` and
/// the command line it is passed to. Both, because Odoo takes some settings
/// only from the file and others only from the flags.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StartupPreview {
    pub conf: String,
    pub command: String,
}

/// One backup file on disk, described well enough to decide whether to
/// restore it without opening it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Backup {
    pub path: String,
    /// The database it was taken from, read out of the filename.
    pub database_name: String,
    pub database_id: Option<Uuid>,
    /// Whether that database is still around. `false` is the interesting
    /// case: it's why this list exists.
    pub source_still_exists: bool,
    /// Whether the attachments were taken too.
    pub has_filestore: bool,
    pub size_bytes: u64,
    pub taken_at: chrono::DateTime<chrono::Utc>,
}

/// Whether a database has had Odoo's neutralization run over it — the
/// difference between a copy that can email a client's customers and one
/// that can't.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NeutralizationState {
    /// Odoo's `database.is_neutralized` flag is set: mail servers off,
    /// crons off, provider credentials cleared.
    Neutralized,
    /// An Odoo database with the flag absent. Live mail servers and crons,
    /// whatever the dump carried.
    Live,
    /// No Odoo has initialized this database, so the question doesn't apply.
    NotOdoo,
    /// Its Postgres isn't running, so nothing could be read. Deliberately
    /// distinct from `Live` — "I couldn't check" must never render as
    /// "it's fine" *or* as "it's dangerous".
    Unknown,
}

/// A registered addons source that will ride along on the migration's
/// addons path — see the doc comment on `run_one_hop` for why this exists
/// and exactly what it does and doesn't promise.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CarriedAddon {
    pub label: String,
    pub kind: SourceKind,
    pub path_or_url: String,
}

/// What migrating one database to a newer Odoo would involve.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UpgradePlan {
    pub database_id: Uuid,
    pub database_name: String,
    pub from: String,
    pub to: String,
    /// The copy the migration would run on. The original is never touched.
    pub into: String,
    pub hops: Vec<openupgrade::Hop>,
    /// The server's own Private/OCA addons sources that will be put on the
    /// addons path for every hop, so a database that actually has a custom
    /// module installed doesn't fail outright. Shown up front because
    /// carrying the code is not the same as migrating it — see
    /// `run_one_hop`.
    pub carried_addons: Vec<CarriedAddon>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FailedHop {
    pub hop: openupgrade::Hop,
    pub message: String,
    /// The snapshot taken immediately before this hop — the way back from a
    /// database that is now neither version.
    pub checkpoint_id: Uuid,
}

/// What actually happened. `failed` being `Some` is a normal outcome, not
/// an error: the hops before it really did succeed, and saying which one
/// stopped is the whole point.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UpgradeRun {
    pub plan: UpgradePlan,
    pub working_database_id: Option<Uuid>,
    pub completed: Vec<openupgrade::Hop>,
    pub failed: Option<FailedHop>,
}

/// Whether one installed module matches the code on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    /// The files on disk hash to what this database was last given.
    Live,
    /// They don't. This module needs updating for the code to take effect.
    Changed,
    /// Installed before this app recorded hashes, or by something else.
    /// Not "fine" — unknown, and said so.
    Unrecorded,
    /// Installed in the database, but its folder isn't on the addons path.
    NotOnDisk,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModuleFreshness {
    pub technical_name: String,
    pub freshness: Freshness,
}

/// One cached, pre-initialized database kept so the next one of the same
/// shape is a copy instead of a boot.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CachedTemplate {
    pub key: String,
    pub database_name: String,
    pub instance_id: Uuid,
    pub size_bytes: u64,
}

/// How many lines of output are kept per Odoo. Enough for a full startup
/// plus a long traceback, small enough that several running Odoos cost
/// megabytes rather than gigabytes.
const LOG_BUFFER_LINES: usize = 5000;

impl Core {
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let core = Self {
            db: Arc::new(Db::open(db_path)?),
            bus: EventBus::new(),
            postgres_supervisors: Arc::new(Mutex::new(HashMap::new())),
            odoo_supervisors: Arc::new(Mutex::new(HashMap::new())),
            runtime_store: Arc::new(Mutex::new(None)),
            data_dir: Arc::new(Mutex::new(None)),
            filestore_dirs: Arc::new(Mutex::new(HashMap::new())),
            log_buffers: Arc::new(Mutex::new(HashMap::new())),
            log_bus: LogBus::new(),
            data_watchers: Arc::new(Mutex::new(HashMap::new())),
        };
        core.adopt_orphan_servers()?;
        Ok(core)
    }

    /// Files any Odoo written before projects existed under a default
    /// project, so the UI's top level is never partially empty in a way that
    /// looks like data loss. A no-op on a database that has none — which is
    /// every freshly-created one — and deliberately silent in that case
    /// rather than recording a migration event nobody needs to see.
    fn adopt_orphan_servers(&self) -> Result<(), CoreError> {
        if self.db.count_orphan_servers()? == 0 {
            return Ok(());
        }
        let project = Project::new("My work", 0);
        self.db.insert_project(&project)?;
        let adopted = self.db.adopt_orphan_servers(project.id)?;
        self.record_event(Event::ProjectCreated {
            project_id: project.id,
            name: format!("{} ({adopted} existing)", project.name),
        })?;
        Ok(())
    }

    // --- projects --------------------------------------------------------

    /// Tell this core where its own folder is, and where to cache and
    /// look for Odoo checkouts. Called once at startup by the shell.
    pub fn with_environment(
        &self,
        data_dir: impl Into<std::path::PathBuf>,
        search_paths: Vec<std::path::PathBuf>,
    ) {
        let data_dir = data_dir.into();
        let store = odoo_runtime::RuntimeStore::new(data_dir.join("odoo")).searching(search_paths);
        *self.runtime_store.lock().expect("runtime store mutex poisoned") = Some(store);
        *self.data_dir.lock().expect("data dir mutex poisoned") = Some(data_dir);
    }

    fn store(&self) -> Result<odoo_runtime::RuntimeStore, CoreError> {
        self.runtime_store
            .lock()
            .expect("runtime store mutex poisoned")
            .clone()
            .ok_or_else(|| CoreError::NotConfigured("no Odoo folder is configured for this app".into()))
    }

    fn app_dir(&self) -> Result<std::path::PathBuf, CoreError> {
        self.data_dir
            .lock()
            .expect("data dir mutex poisoned")
            .clone()
            .ok_or_else(|| CoreError::NotConfigured("no data folder is configured for this app".into()))
    }

    /// Where a version of Odoo is, without downloading anything — the
    /// cache, then checkouts already on this machine. `None` means it
    /// would have to be fetched.
    pub fn find_odoo(&self, version: &str) -> Result<Option<odoo_runtime::OdooRuntime>, CoreError> {
        Ok(self.store()?.resolve(version))
    }

    /// As `find_odoo`, but fetches the series if nothing was found. One
    /// copy per version is shared by every project that wants it.
    pub async fn ensure_odoo(&self, version: &str) -> Result<odoo_runtime::OdooRuntime, CoreError> {
        Ok(self.store()?.ensure(version).await?)
    }

    /// The database store everything uses, creating and starting one in
    /// the app's own folder if there isn't one yet. Nobody should have to
    /// choose a folder or a port to make their first database, so this is
    /// what every "which storage?" question resolves to by default.
    pub async fn ensure_storage(&self) -> Result<PostgresInstance, CoreError> {
        if let Some(existing) = self.list_postgres_instances()?.into_iter().next() {
            if !matches!(self.postgres_instance_state(existing.id)?, postgres::PgState::Running { .. }) {
                self.start_postgres_instance(existing.id).await?;
            }
            return self.get_postgres_instance(existing.id);
        }
        let dir = self.app_dir()?.join("databases");
        std::fs::create_dir_all(&dir)?;
        // A port has to be chosen, but not by a person: Postgres is told an
        // explicit port on the command line, so unlike a TCP listener it
        // can't be handed 0 and asked to pick. Bind an ephemeral socket,
        // note what the OS gave out, and let it go.
        let port = free_local_port()?;
        let created = self.create_postgres_instance("Local databases", "16", port, dir.to_string_lossy().to_string())?;
        self.start_postgres_instance(created.id).await?;
        self.get_postgres_instance(created.id)
    }

    /// One entry per database: the hostname it answers on, the Odoo
    /// listening for it, and the database name that Odoo's own `dbfilter`
    /// needs to see in the Host header.
    ///
    /// This is what makes both supported shapes the same mechanism. One
    /// Odoo serving several databases and one Odoo per database differ
    /// only in how many rows here share a `port` — nothing else in the
    /// system has to know which shape a project is using.
    pub fn routing_table(&self) -> Result<Vec<Route>, CoreError> {
        let servers = self.db.list_servers()?;
        let mut routes = Vec::new();
        for database in self.db.list_databases(None)? {
            let Some(server) = servers.iter().find(|s| s.id == database.server_id) else {
                continue;
            };
            routes.push(Route {
                host: database.hostname(),
                port: server.port,
                database: database.name.clone(),
                server_id: server.id,
                database_id: database.id,
                running: matches!(server.state, ServerState::Running),
            });
        }
        Ok(routes)
    }

    /// Give a database its own hostname, or clear it back to the default
    /// `<name>.odoo`. Refuses a hostname another database already answers
    /// on — two databases behind one name is a silent mis-route, and
    /// finding out by logging into the wrong client's books is exactly the
    /// failure this whole product exists to prevent.
    pub fn set_database_domain(&self, database_id: Uuid, domain: Option<&str>) -> Result<Database, CoreError> {
        self.get_database(database_id)?;
        if let Some(domain) = domain {
            let domain = domain.trim().to_lowercase();
            if domain.is_empty() {
                return Err(CoreError::NotConfigured("a domain can't be blank".into()));
            }
            if let Some(clash) = self
                .routing_table()?
                .into_iter()
                .find(|r| r.host == domain && r.database_id != database_id)
            {
                return Err(CoreError::DomainTaken { domain, database: clash.database });
            }
            self.db.set_database_domain(database_id, Some(&domain))?;
        } else {
            self.db.set_database_domain(database_id, None)?;
        }
        self.get_database(database_id)
    }

    // --- reconciliation, sizes and disk ------------------------------------
    //
    // The app's rows and the real cluster drift. Someone runs `createdb` by
    // hand, or restores a dump outside the app, or drops something with
    // psql at 2am. A tool that quietly shows its own stale opinion of
    // reality while touching client data is worse than no tool, so this
    // section is about asking Postgres what is actually there and showing
    // both directions of the difference.

    /// Every database the *cluster* actually has, with its real size —
    /// Postgres's own answer, not this app's rows. Template and system
    /// databases are excluded: they aren't anybody's work.
    pub async fn list_cluster_databases(&self, instance_id: Uuid) -> Result<Vec<ClusterDatabase>, CoreError> {
        let conn = self.pg_conn_info_for_running_instance(instance_id)?;
        let rows = pg_admin::query_rows(
            &conn,
            "postgres",
            "SELECT datname, pg_database_size(datname) \
             FROM pg_database \
             WHERE NOT datistemplate AND datname <> 'postgres' \
             ORDER BY datname",
        )
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                let name = row.first()?.clone();
                let size_bytes = row.get(1).and_then(|v| v.parse().ok()).unwrap_or(0);
                Some(ClusterDatabase { instance_id, name, size_bytes })
            })
            .collect())
    }

    /// Compares this app's rows against every running database store and
    /// reports the difference in both directions. Stores that aren't
    /// running are reported as `unchecked` rather than having their
    /// databases silently counted as missing — "I couldn't look" and
    /// "it's gone" are very different claims and only one of them should
    /// make someone reach for a backup.
    ///
    /// Snapshot databases are excluded from `untracked`: they're real
    /// databases the app made on purpose, and listing them as strays
    /// would train people to ignore this screen.
    pub async fn reconcile_databases(&self) -> Result<Reconciliation, CoreError> {
        let tracked = self.db.list_databases(None)?;
        let servers = self.db.list_servers()?;
        let snapshot_names: std::collections::HashSet<String> =
            self.db.list_snapshots(None)?.into_iter().map(|s| s.snapshot_database_name).collect();

        let mut untracked = Vec::new();
        let mut missing = Vec::new();
        let mut unchecked = Vec::new();

        for instance in self.db.list_postgres_instances()? {
            let real = match self.list_cluster_databases(instance.id).await {
                Ok(real) => real,
                Err(_) => {
                    unchecked.push(instance.label.clone());
                    continue;
                }
            };
            let real_names: std::collections::HashSet<&str> = real.iter().map(|d| d.name.as_str()).collect();

            // Which of this app's rows point at this store, via the Odoo
            // that owns them.
            let servers_here: Vec<Uuid> =
                servers.iter().filter(|s| s.postgres_instance_id == instance.id).map(|s| s.id).collect();
            let tracked_here: Vec<&Database> =
                tracked.iter().filter(|d| servers_here.contains(&d.server_id)).collect();
            let tracked_names: std::collections::HashSet<&str> =
                tracked_here.iter().map(|d| d.name.as_str()).collect();

            for database in &tracked_here {
                if !real_names.contains(database.name.as_str()) {
                    missing.push(MissingDatabase {
                        database_id: database.id,
                        name: database.name.clone(),
                        server_id: database.server_id,
                        store: instance.label.clone(),
                    });
                }
            }
            for found in real {
                // Snapshots and cached templates are this app's own
                // bookkeeping, not databases somebody made. Listing them as
                // "found in Postgres but unknown to this app" would be a
                // reconciliation report that cries wolf about its own
                // filing cabinet.
                let ours = snapshot_names.contains(&found.name) || found.name.starts_with("oo_template_");
                if !tracked_names.contains(found.name.as_str()) && !ours {
                    untracked.push(found);
                }
            }
        }

        Ok(Reconciliation { untracked, missing, unchecked })
    }

    /// Re-reads every tracked database's real size from its cluster and
    /// stores it. Returns how many rows were updated — a store that isn't
    /// running simply contributes none, and says nothing false in the
    /// meantime.
    pub async fn refresh_database_sizes(&self) -> Result<u32, CoreError> {
        let tracked = self.db.list_databases(None)?;
        let servers = self.db.list_servers()?;
        let mut updated = 0;
        for instance in self.db.list_postgres_instances()? {
            let Ok(real) = self.list_cluster_databases(instance.id).await else { continue };
            let servers_here: Vec<Uuid> =
                servers.iter().filter(|s| s.postgres_instance_id == instance.id).map(|s| s.id).collect();
            for database in tracked.iter().filter(|d| servers_here.contains(&d.server_id)) {
                if let Some(found) = real.iter().find(|r| r.name == database.name) {
                    self.db.set_database_size(database.id, found.size_bytes)?;
                    updated += 1;
                }
            }
        }
        Ok(updated)
    }

    /// Adopts a database that already exists in a store onto an Odoo, so
    /// the app stops pretending it isn't there. Deliberately does *not*
    /// create anything: this is the "yes, that one is mine" button next to
    /// something Postgres already reported.
    pub async fn adopt_database(&self, server_id: Uuid, name: impl Into<String>) -> Result<Database, CoreError> {
        let server = self.get_server(server_id)?;
        let name = name.into();
        let conn = self.pg_conn_info_for_running_instance(server.postgres_instance_id)?;
        if !pg_admin::database_exists(&conn, &name).await? {
            return Err(CoreError::NotFound(format!("database {name} doesn't exist in that store")));
        }
        if self.db.list_databases(None)?.iter().any(|d| d.name == name && d.server_id == server_id) {
            return Err(CoreError::NotConfigured(format!("{name} is already known to this app")));
        }
        // Adopted, not created: this database already existed in the
        // cluster and nothing here knows what made it or what's in it.
        let database = Database::from_origin(server_id, name.clone(), model::DatabaseOrigin::Adopted);
        self.db.insert_database(&database)?;
        self.record_event(Event::DatabaseCreated { database_id: database.id, server_id, name })?;
        Ok(database)
    }

    /// Removes a metadata row **without touching Postgres** — the cleanup
    /// for a row whose database is genuinely gone. Distinct from
    /// `drop_database` on purpose: one deletes data, the other admits data
    /// was already deleted, and conflating them is how a "tidy up" click
    /// destroys a database.
    pub fn forget_database(&self, database_id: Uuid) -> Result<(), CoreError> {
        let database = self.get_database(database_id)?;
        self.db.delete_database(database_id)?;
        self.record_event(Event::DatabaseDropped {
            database_id,
            server_id: database.server_id,
            name: database.name,
        })?;
        Ok(())
    }

    /// Where the disk went. Every figure is measured, not estimated; a
    /// folder that doesn't exist yet contributes zero rather than an
    /// error.
    pub fn disk_usage(&self) -> Result<DiskUsage, CoreError> {
        let app_dir = self.app_dir()?;
        let databases: u64 = self.db.list_databases(None)?.iter().map(|d| d.size_bytes).sum();
        let snapshots: u64 = self
            .db
            .list_snapshots(None)?
            .iter()
            .filter_map(|s| s.filestore_snapshot_path.as_ref())
            .map(|p| odoo_runtime::directory_size(std::path::Path::new(p)))
            .sum();
        let odoo_copies = self.cached_odoo_copies().unwrap_or_default();
        Ok(DiskUsage {
            database_bytes: databases,
            snapshot_filestore_bytes: snapshots,
            backup_bytes: odoo_runtime::directory_size(&app_dir.join("backups")),
            module_bytes: odoo_runtime::directory_size(&app_dir.join("modules")),
            odoo_copy_bytes: odoo_copies.iter().map(|c| c.size_bytes).sum(),
            odoo_copies,
        })
    }

    // --- filestores ---------------------------------------------------------
    //
    // A snapshot that copies the database and not the filestore is not a
    // point-in-time copy of someone's work: Odoo keeps attachments —
    // invoice PDFs, product images, uploaded documents — as files on disk,
    // with only their metadata in Postgres. Restoring one without the
    // other gives back rows that point at files which have since changed,
    // and silently loses files created since. For a tool whose whole
    // promise is "put it back exactly as it was", that is the worst
    // possible failure: it looks like it worked.
    //
    // So the filestore path is **derived, never asked for**. Nothing in
    // the UI should be able to take a half-snapshot by forgetting an
    // optional argument, which is exactly what was happening.

    /// Where Odoo keeps this database's attachments.
    ///
    /// `start_server` writes `data_dir = <runtime>/filestore` into the
    /// generated `odoo.conf`, and Odoo itself then stores each database's
    /// files under `<data_dir>/filestore/<database name>` — that second
    /// `filestore` segment is Odoo's own layout, not a typo here.
    ///
    /// Prefers the directory a running `start_server` actually configured,
    /// falling back to the standard layout under the app's own folder.
    /// The two agree for every path the app itself uses; they can differ
    /// only when a caller passed an explicit `runtime_dir` (tests do), and
    /// remembering the real one means a snapshot taken in that case is
    /// still correct rather than quietly empty.
    ///
    /// `None` means there's nowhere it could be — no data directory is
    /// configured — not that the database has no attachments.
    pub fn database_filestore_path(&self, database_id: Uuid) -> Result<Option<std::path::PathBuf>, CoreError> {
        let database = self.get_database(database_id)?;
        let remembered = self
            .filestore_dirs
            .lock()
            .expect("filestore dirs mutex poisoned")
            .get(&database.server_id)
            .cloned();
        let root = match remembered {
            Some(dir) => dir,
            None => match self.data_dir.lock().expect("data dir mutex poisoned").clone() {
                Some(data_dir) => data_dir.join("run").join(database.server_id.to_string()).join("filestore"),
                None => return Ok(None),
            },
        };
        Ok(Some(root.join("filestore").join(&database.name)))
    }

    /// Where snapshot filestore copies live. Derived for the same reason
    /// as the path above: a caller that forgets it takes a half-snapshot.
    fn snapshots_dir(&self) -> Result<Option<std::path::PathBuf>, CoreError> {
        Ok(self.data_dir.lock().expect("data dir mutex poisoned").clone().map(|d| d.join("snapshots")))
    }

    /// Every copy of Odoo this app has downloaded, with its real size.
    pub fn cached_odoo_copies(&self) -> Result<Vec<odoo_runtime::CachedRuntime>, CoreError> {
        Ok(self.store()?.cached())
    }

    /// Deletes one downloaded copy of Odoo, returning the bytes freed.
    /// Only ever touches this app's own cache folder — a checkout adopted
    /// from somewhere else on the machine belongs to the user.
    pub fn delete_cached_odoo(&self, folder: &str) -> Result<u64, CoreError> {
        Ok(self.store()?.remove_cached(folder)?)
    }

    /// Register a folder of modules, cloning it first when given a git URL
    /// rather than a path. The clone lives beside the Odoo it belongs to,
    /// so removing that Odoo takes its code with it.
    pub async fn add_modules_from(
        &self,
        server_id: Uuid,
        label: impl Into<String>,
        path_or_url: impl Into<String>,
        kind: SourceKind,
        rank: u32,
    ) -> Result<AddonsSource, CoreError> {
        let label = label.into();
        let path_or_url = path_or_url.into();
        let resolved = if looks_like_git_url(&path_or_url) {
            let target = self.app_dir()?.join("modules").join(server_id.to_string()).join(slugify(&label));
            let url = path_or_url.clone();
            let dest = target.clone();
            tokio::task::spawn_blocking(move || git::clone(&url, &dest))
                .await
                .expect("module clone task panicked")?;
            target.to_string_lossy().to_string()
        } else {
            path_or_url
        };
        self.create_addons_source(server_id, label, resolved, kind, rank)
    }

    pub fn create_project(&self, name: impl Into<String>, color: u8) -> Result<Project, CoreError> {
        let project = Project::new(name, color);
        self.db.insert_project(&project)?;
        self.record_event(Event::ProjectCreated {
            project_id: project.id,
            name: project.name.clone(),
        })?;
        Ok(project)
    }

    pub fn list_projects(&self) -> Result<Vec<Project>, CoreError> {
        self.db.list_projects()
    }

    pub fn get_project(&self, project_id: Uuid) -> Result<Project, CoreError> {
        self.db.get_project(project_id)?.ok_or_else(|| CoreError::NotFound(format!("project {project_id}")))
    }

    pub fn rename_project(&self, project_id: Uuid, name: impl Into<String>, color: u8) -> Result<Project, CoreError> {
        let existing = self.get_project(project_id)?;
        let name = name.into();
        self.db.update_project(project_id, &name, color % 6)?;
        self.record_event(Event::ProjectRenamed { project_id, name: name.clone() })?;
        Ok(Project { name, color: color % 6, ..existing })
    }

    /// Refuses while the project still holds Odoos, rather than cascading
    /// into somebody's databases. Emptying it is a series of individually
    /// visible deletions the user makes deliberately — the friction ladder
    /// in `odoo-orchestrator-ui-design-principles.md` puts "delete instance
    /// including volumes" at its highest rung, and a project delete that
    /// silently performed several of those at once would be the exact
    /// blast-radius mismatch that ladder exists to prevent.
    pub fn delete_project(&self, project_id: Uuid) -> Result<(), CoreError> {
        self.get_project(project_id)?;
        let remaining = self.db.count_servers_in_project(project_id)?;
        if remaining > 0 {
            return Err(CoreError::ProjectNotEmpty { project_id, server_count: remaining });
        }
        self.db.delete_project(project_id)?;
        self.record_event(Event::ProjectDeleted { project_id })?;
        Ok(())
    }

    /// Copies a project's **setup** — each Odoo's version, Postgres
    /// instance and addons path — onto a fresh project, giving every copy a
    /// port nothing else is using. Databases are deliberately *not* copied:
    /// duplicating a client's data by pressing one button is exactly the
    /// kind of quiet, expensive surprise this project's design rules exist
    /// to prevent, and a database copy is already its own explicit action
    /// (`duplicate_database`). Callers should say so in the UI rather than
    /// letting a user infer it.
    pub fn duplicate_project(&self, project_id: Uuid, new_name: impl Into<String>) -> Result<Project, CoreError> {
        let source = self.get_project(project_id)?;
        let copy = Project::new(new_name, source.color);
        self.db.insert_project(&copy)?;

        let mut used: Vec<u16> = self.db.used_ports()?;
        let mut copied = 0u32;
        for server in self.db.list_servers()?.into_iter().filter(|s| s.project_id == Some(project_id)) {
            let port = next_free_port(server.port, &used);
            used.push(port);
            let new_server = OdooServer::new(server.name.clone(), server.odoo_version.clone(), port, server.postgres_instance_id)
                .in_project(copy.id);
            self.db.insert_server(&new_server)?;
            for source_entry in self.db.list_addons_sources(server.id)? {
                self.db.insert_addons_source(&AddonsSource {
                    id: Uuid::new_v4(),
                    server_id: new_server.id,
                    label: source_entry.label,
                    path_or_url: source_entry.path_or_url,
                    kind: source_entry.kind,
                    rank: source_entry.rank,
                })?;
            }
            copied += 1;
        }

        self.record_event(Event::ProjectDuplicated {
            project_id: copy.id,
            source_project_id: project_id,
            server_count: copied,
        })?;
        Ok(copy)
    }

    /// Move an Odoo to another project. Purely organizational — nothing
    /// about how it runs changes, which is why there's no process work here.
    pub fn move_server_to_project(&self, server_id: Uuid, project_id: Uuid) -> Result<OdooServer, CoreError> {
        self.get_server(server_id)?;
        self.get_project(project_id)?;
        self.db.set_server_project(server_id, project_id)?;
        self.get_server(server_id)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.bus.subscribe()
    }

    /// Persist an event, THEN broadcast it — never the other way around, so
    /// no subscriber ever observes an event that isn't already durable.
    fn record_event(&self, event: Event) -> Result<EventEnvelope, CoreError> {
        self.append(EventEnvelope::new(event))
    }

    /// Record an event along with how long the operation took. Used for
    /// the operations that genuinely take time, so the Stats screen can
    /// state real medians rather than estimates.
    fn record_timed_event(&self, event: Event, took: std::time::Duration) -> Result<EventEnvelope, CoreError> {
        self.append(EventEnvelope::timed(event, took))
    }

    fn append(&self, envelope: EventEnvelope) -> Result<EventEnvelope, CoreError> {
        self.db.append_event(&envelope)?;
        self.bus.publish(envelope.clone());
        Ok(envelope)
    }

    pub fn recent_events(&self, limit: u32) -> Result<Vec<EventEnvelope>, CoreError> {
        self.db.recent_events(limit)
    }

    // --- servers ---------------------------------------------------------

    /// `postgres_instance_id` must already exist (created via
    /// `create_postgres_instance`) — fails loudly with `NotFound` rather
    /// than letting a bad id surface later as a confusing error the first
    /// time something tries to actually use it. This is where the
    /// previously-open "which `PostgresInstance` does a server use"
    /// decision from task 2.1/2.2 is exercised: the caller decides, per the
    /// topology rule (share by default, separate only when something
    /// genuinely forces it) — `Core` doesn't guess or auto-assign.
    pub fn create_server(
        &self,
        name: impl Into<String>,
        odoo_version: impl Into<String>,
        port: u16,
        postgres_instance_id: Uuid,
    ) -> Result<OdooServer, CoreError> {
        self.create_server_in_project(name, odoo_version, port, postgres_instance_id, None)
    }

    /// As `create_server`, but files the new Odoo under a project. The
    /// project must already exist — same posture as `postgres_instance_id`
    /// above: fail loudly now rather than let a dangling id surface later as
    /// an Odoo that belongs nowhere and so is invisible in the UI.
    pub fn create_server_in_project(
        &self,
        name: impl Into<String>,
        odoo_version: impl Into<String>,
        port: u16,
        postgres_instance_id: Uuid,
        project_id: Option<Uuid>,
    ) -> Result<OdooServer, CoreError> {
        self.get_postgres_instance(postgres_instance_id)?;
        let mut server = OdooServer::new(name, odoo_version, port, postgres_instance_id);
        if let Some(project_id) = project_id {
            self.get_project(project_id)?;
            server = server.in_project(project_id);
        }
        self.db.insert_server(&server)?;
        self.record_event(Event::ServerCreated {
            server_id: server.id,
            name: server.name.clone(),
            odoo_version: server.odoo_version.clone(),
            postgres_instance_id,
        })?;
        Ok(server)
    }

    pub fn list_servers(&self) -> Result<Vec<OdooServer>, CoreError> {
        self.db.list_servers()
    }

    pub fn get_server(&self, server_id: Uuid) -> Result<OdooServer, CoreError> {
        self.db.get_server(server_id)?.ok_or_else(|| CoreError::NotFound(format!("server {server_id}")))
    }

    /// Register an addons-path entry for a server, in load order (`rank` 0
    /// loads first and wins any technical-name collision). Returns the
    /// created source so the caller (the API layer) has its generated id
    /// without a round trip.
    pub fn create_addons_source(
        &self,
        server_id: Uuid,
        label: impl Into<String>,
        path_or_url: impl Into<String>,
        kind: SourceKind,
        rank: u32,
    ) -> Result<AddonsSource, CoreError> {
        // Fails loudly (NotFound) rather than letting a bad server_id surface
        // as an opaque SQLite foreign-key error later.
        self.get_server(server_id)?;
        let source = AddonsSource { id: Uuid::new_v4(), server_id, label: label.into(), path_or_url: path_or_url.into(), kind, rank };
        self.db.insert_addons_source(&source)?;
        Ok(source)
    }

    pub fn list_addons_sources(&self, server_id: Uuid) -> Result<Vec<AddonsSource>, CoreError> {
        self.db.list_addons_sources(server_id)
    }

    /// The server's own addons sources that a migration should carry along —
    /// everything except `Core`, which every hop already gets from its own
    /// Odoo checkout. See `run_one_hop`'s doc comment for what carrying the
    /// code does and doesn't promise.
    fn carried_addons_for(&self, server_id: Uuid) -> Result<Vec<CarriedAddon>, CoreError> {
        Ok(self
            .list_addons_sources(server_id)?
            .into_iter()
            .filter(|s| s.kind != SourceKind::Core)
            .map(|s| CarriedAddon { label: s.label, kind: s.kind, path_or_url: s.path_or_url })
            .collect())
    }

    /// Walk every addons source configured for `server_id`, parse every
    /// module's manifest, and resolve addons-path shadowing + a topological
    /// install order across all of them — see `modules.rs`. Recomputed fresh
    /// on every call (no cache yet); records a `ModulesScanned` event so the
    /// scan itself shows up in the activity log like any other action.
    pub fn scan_modules(&self, server_id: Uuid) -> Result<ModuleScan, CoreError> {
        self.get_server(server_id)?;
        let sources = self.db.list_addons_sources(server_id)?;
        let scan = modules::scan(server_id, &sources);
        self.record_event(Event::ModulesScanned {
            server_id,
            module_count: scan.modules.len() as u32,
            shadow_count: scan.collisions.len() as u32,
        })?;
        Ok(scan)
    }

    // --- databases ---------------------------------------------------------
    //
    // Task 2.3. Real Postgres operations (`pg_admin`) against whichever
    // `PostgresInstance` the database's server is assigned to — every
    // method here requires that instance to actually be `Running` first
    // (returns `PostgresInstanceNotRunning` otherwise) since there's a real
    // `createdb`/`pg_dump`/etc. call to make, not just a SQLite row to
    // write. This is a genuine gap the previous session's `create_database`
    // had — it only wrote a `Database` metadata row and never touched
    // Postgres at all, which was itself a small "no fake progress" lapse
    // this fixes.

    /// Connection info for a *running* Postgres instance's client tools —
    /// separated from `get_or_create_supervisor` because callers here don't
    /// need (and shouldn't accidentally trigger) supervisor registration;
    /// they need a server that's already up.
    fn pg_conn_info_for_running_instance(&self, id: Uuid) -> Result<pg_admin::PgConnInfo, CoreError> {
        let instance = self.get_postgres_instance(id)?;
        let state = self.postgres_instance_state(id)?;
        if !matches!(state, PgState::Running { .. }) {
            return Err(CoreError::PostgresInstanceNotRunning { instance_id: id, state });
        }
        let bin_dir = postgres::discover_system_bin_dir().ok_or_else(|| {
            CoreError::NotFound(
                "no Postgres binaries found (dev/test environments need a system Postgres install; \
                 the shipped app bundles its own runtime instead)"
                    .to_string(),
            )
        })?;
        Ok(pg_admin::PgConnInfo::new(bin_dir, instance.data_dir.clone(), instance.port))
    }

    /// Creates a real Postgres database on `server_id`'s assigned instance,
    /// then records the `Database` row. If the create-database step fails
    /// (instance not running, name collision, etc.), nothing is written to
    /// SQLite — real Postgres state and the metadata row never disagree.
    pub async fn create_database(&self, server_id: Uuid, name: impl Into<String>) -> Result<Database, CoreError> {
        let started = std::time::Instant::now();
        let server = self.get_server(server_id)?;
        let name = name.into();
        let conn = self.pg_conn_info_for_running_instance(server.postgres_instance_id)?;
        pg_admin::create_database(&conn, &name).await?;

        let database = Database::new(server_id, name);
        self.db.insert_database(&database)?;
        self.record_timed_event(
            Event::DatabaseCreated { database_id: database.id, server_id, name: database.name.clone() },
            started.elapsed(),
        )?;
        Ok(database)
    }

    pub fn list_databases(&self, server_id: Option<Uuid>) -> Result<Vec<Database>, CoreError> {
        self.db.list_databases(server_id)
    }

    pub fn get_database(&self, database_id: Uuid) -> Result<Database, CoreError> {
        self.db
            .list_databases(None)?
            .into_iter()
            .find(|d| d.id == database_id)
            .ok_or_else(|| CoreError::NotFound(format!("database {database_id}")))
    }

    /// Force-disconnects every session on the source database, then
    /// `CREATE DATABASE … TEMPLATE …`s a real copy — see `pg_admin`'s
    /// module doc comment for why both steps are required. Registers the
    /// copy as its own `Database` row on the same server.
    pub async fn duplicate_database(&self, database_id: Uuid, new_name: impl Into<String>) -> Result<Database, CoreError> {
        let started = std::time::Instant::now();
        let source = self.get_database(database_id)?;
        let server = self.get_server(source.server_id)?;
        let new_name = new_name.into();
        let conn = self.pg_conn_info_for_running_instance(server.postgres_instance_id)?;
        pg_admin::duplicate_database(&conn, &source.name, &new_name).await?;

        let copy = Database::from_origin(source.server_id, new_name, model::DatabaseOrigin::Duplicated);
        self.db.insert_database(&copy)?;
        self.record_timed_event(Event::DatabaseDuplicated { database_id: copy.id, source_database_id: database_id }, started.elapsed())?;
        Ok(copy)
    }

    /// Drops the real Postgres database and removes its metadata row.
    /// **Does not** clean up an Odoo filestore directory — there's no real
    /// Odoo filestore anywhere in this codebase yet to know the layout of
    /// (see `odoo.rs`'s module doc comment); the runtime-architecture doc's
    /// "dropping via plain `dropdb` leaves an orphan filestore" gotcha is
    /// still open once a real filestore convention exists to clean up.
    pub async fn drop_database(&self, database_id: Uuid) -> Result<(), CoreError> {
        let database = self.get_database(database_id)?;
        let server = self.get_server(database.server_id)?;
        let conn = self.pg_conn_info_for_running_instance(server.postgres_instance_id)?;
        // Read the path before the row goes away — it's derived from the
        // database's own name and server.
        let filestore = self.database_filestore_path(database_id)?;
        pg_admin::drop_database(&conn, &database.name).await?;

        // Dropping the database and leaving its attachments on disk is the
        // orphan-filestore gotcha the runtime-architecture doc warns about:
        // invisible, counted by nothing, and eventually a real slice of the
        // disk the Stats screen reports. Deliberately *after* the database
        // is really gone, and deliberately not fatal — a failure to tidy up
        // must not make the caller think the drop itself failed.
        if let Some(filestore) = filestore.filter(|p| p.is_dir()) {
            if let Err(err) = std::fs::remove_dir_all(&filestore) {
                tracing::warn!("dropped {} but couldn't remove its filestore at {}: {err}", database.name, filestore.display());
            }
        }

        self.db.delete_database(database_id)?;
        // Otherwise a later database that reused this id would inherit a
        // dropped one's idea of which modules are up to date.
        self.db.forget_module_installs(database_id)?;
        self.record_event(Event::DatabaseDropped {
            database_id,
            server_id: database.server_id,
            name: database.name.clone(),
        })?;
        Ok(())
    }

    /// Plain-SQL dump via `pg_dump`, written to `dest_dir/<name>-<id>.sql`.
    /// This is the "dump" format from the runtime-architecture doc (no
    /// filestore) — see `pg_admin`'s module doc comment for why the "zip"
    /// format (dump + filestore) isn't implemented. Returns the file path
    /// so the caller (eventually the API layer) can offer it as a download.
    pub async fn backup_database(&self, database_id: Uuid, dest_dir: impl AsRef<std::path::Path>) -> Result<std::path::PathBuf, CoreError> {
        let started = std::time::Instant::now();
        let database = self.get_database(database_id)?;
        let server = self.get_server(database.server_id)?;
        let conn = self.pg_conn_info_for_running_instance(server.postgres_instance_id)?;

        std::fs::create_dir_all(dest_dir.as_ref())?;
        let dest_file = dest_dir.as_ref().join(format!("{}-{}.sql", database.name, database_id));
        pg_admin::dump_database(&conn, &database.name, &dest_file).await?;

        // The attachments, beside the dump. A backup taken "before
        // something scary" that restores the rows but not the invoice PDFs
        // they point at is the same broken promise a database-only
        // snapshot was — and this one is worse, because a backup is what
        // someone reaches for when the snapshot didn't save them.
        //
        // Copied rather than hardlinked: a backup has to survive being
        // moved to another disk or kept after the original is deleted.
        // Named from the dump so the two travel together and it's obvious
        // which belongs to which.
        let filestore_dest = dest_file.with_extension("filestore");
        let took_filestore = match self.database_filestore_path(database_id)?.filter(|p| p.is_dir()) {
            Some(source) => {
                let dest = filestore_dest.clone();
                tokio::task::spawn_blocking(move || filestore::copy_directory(&source, &dest))
                    .await
                    .expect("filestore backup task panicked")?;
                true
            }
            None => false,
        };

        self.db.mark_database_backed_up(database_id, chrono::Utc::now())?;
        self.record_timed_event(
            Event::DatabaseBackedUp {
                database_id,
                server_id: database.server_id,
                // Says what was actually taken, so a restore — and anyone
                // reading the log later — knows whether the files are in
                // there without going and looking.
                format: if took_filestore { "dump+filestore".to_string() } else { "dump".to_string() },
                path: dest_file.to_string_lossy().to_string(),
            },
            started.elapsed(),
        )?;
        Ok(dest_file)
    }

    /// Every backup this app has taken, newest first.
    ///
    /// Exists because "Back up" was writing dumps nothing in the app could
    /// ever find again — `restore_database` was reachable only by someone
    /// who knew the file path, which is nobody. A backup you can't restore
    /// from inside the tool that took it is a button, not a safety net.
    ///
    /// Reads the folder rather than a table on purpose: the dumps are
    /// plain files a person may copy, move or delete outside this app, and
    /// a list built from a database would confidently name files that
    /// aren't there any more.
    pub fn list_backups(&self, dir: impl AsRef<std::path::Path>) -> Result<Vec<Backup>, CoreError> {
        let dir = dir.as_ref();
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        // Names are matched back to live databases where possible, but a
        // backup of a database that has since been dropped is exactly when
        // someone needs this list — so an unmatched one is still listed,
        // named from its own filename.
        let databases = self.list_databases(None)?;
        let mut found = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("sql") {
                continue;
            }
            let file_name = path.file_stem().and_then(|n| n.to_str()).unwrap_or("").to_string();
            // `backup_database` writes `<name>-<uuid>.sql`, so the id is
            // the last 36 characters and the name is whatever precedes it.
            let (database_name, database_id) = match file_name.rsplit_once('-') {
                Some((name, tail)) if Uuid::parse_str(tail).is_ok() => {
                    (name.to_string(), Uuid::parse_str(tail).ok())
                }
                // Split on the last dash only finds the final uuid group;
                // try the full 36-character tail before giving up.
                _ if file_name.len() > 37 => {
                    let (name, tail) = file_name.split_at(file_name.len() - 37);
                    match Uuid::parse_str(tail.trim_start_matches('-')) {
                        Ok(id) => (name.trim_end_matches('-').to_string(), Some(id)),
                        Err(_) => (file_name.clone(), None),
                    }
                }
                _ => (file_name.clone(), None),
            };
            let meta = entry.metadata()?;
            let taken_at = meta
                .modified()
                .ok()
                .map(chrono::DateTime::<chrono::Utc>::from)
                .unwrap_or_else(chrono::Utc::now);
            found.push(Backup {
                path: path.to_string_lossy().to_string(),
                database_name: database_name.clone(),
                database_id,
                // A backup of a dropped database is still restorable; it
                // just has no live row to point at.
                source_still_exists: database_id.is_some_and(|id| databases.iter().any(|d| d.id == id)),
                // The attachments travel beside the dump. Saying so here
                // means nobody restores rows and discovers the invoice PDFs
                // are missing afterwards.
                has_filestore: path.with_extension("filestore").is_dir(),
                size_bytes: meta.len(),
                taken_at,
            });
        }
        found.sort_by(|a, b| b.taken_at.cmp(&a.taken_at));
        Ok(found)
    }

    /// Restores a dump produced by `backup_database` into a brand-new
    /// database on the same server, registered as its own `Database` row.
    pub async fn restore_database(
        &self,
        server_id: Uuid,
        new_name: impl Into<String>,
        dump_file: impl AsRef<std::path::Path>,
    ) -> Result<Database, CoreError> {
        let started = std::time::Instant::now();
        let server = self.get_server(server_id)?;
        let new_name = new_name.into();
        let conn = self.pg_conn_info_for_running_instance(server.postgres_instance_id)?;
        pg_admin::restore_database(&conn, &new_name, dump_file.as_ref()).await?;

        // Restored, which is the origin that matters: a dump can carry a
        // client's live mail servers, payment credentials and real people's
        // data in from somewhere this machine has never seen.
        let database = Database::from_origin(server_id, new_name, model::DatabaseOrigin::Restored);
        self.db.insert_database(&database)?;

        // If the backup carried a filestore, put it back too — under the
        // *new* database's name, since that's where Odoo will look for it.
        // Restoring the rows without the files would hand someone a
        // database full of attachments that 404.
        let backup_filestore = dump_file.as_ref().with_extension("filestore");
        if backup_filestore.is_dir() {
            if let Some(target) = self.database_filestore_path(database.id)? {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let source = backup_filestore.clone();
                let dest = target.clone();
                tokio::task::spawn_blocking(move || filestore::copy_directory(&source, &dest))
                    .await
                    .expect("filestore restore task panicked")?;
            }
        }

        self.record_timed_event(Event::DatabaseRestored { database_id: database.id }, started.elapsed())?;
        Ok(database)
    }

    // --- snapshots (task 3.1) -----------------------------------------------
    //
    // The branch-based-snapshotting headline feature's first primitive:
    // instant, named copies of a database (and, when a filestore path is
    // given, a directory tree alongside it). Deliberately built as two
    // independently-real, independently-tested halves reused rather than
    // reimplemented: the database half is exactly 2.3's own
    // `pg_admin::duplicate_database` (force-disconnect, then
    // `CREATE DATABASE ... TEMPLATE`), and the filestore half is the new
    // `filestore::snapshot_directory` hardlink primitive. Git-branch binding
    // (3.2) and one-click revert (3.3) build on top of this; this task only
    // proves the primitive itself works against real Postgres and a real
    // directory tree.

    /// Snapshot database names need to be both valid Postgres identifiers
    /// and collision-free across repeated snapshots of the same source, so
    /// this derives one from the source name plus a fresh UUID rather than
    /// trusting the caller's snapshot `name` (which is free-form user text,
    /// not necessarily identifier-safe, and not necessarily unique).
    fn derive_snapshot_database_name(source_database_name: &str) -> String {
        let sanitized: String = source_database_name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
            .collect();
        format!("snap_{}_{}", sanitized, Uuid::new_v4().simple())
    }

    /// Takes a real snapshot: duplicates `database_id`'s Postgres database
    /// (via 2.3's `duplicate_database` mechanism) under a fresh, derived
    /// name, and — only when both `filestore_source` and
    /// `filestore_snapshots_dir` are given — hardlink-snapshots that
    /// directory tree too via `filestore::snapshot_directory`. Passing
    /// `None` for the filestore is the honest, common case today (see
    /// `Snapshot`'s doc comment): there's no real Odoo filestore in this
    /// project yet, so most snapshots taken now are Postgres-only, and nothing
    /// pretends otherwise.
    pub async fn create_snapshot(
        &self,
        database_id: Uuid,
        name: impl Into<String>,
        filestore_source: Option<std::path::PathBuf>,
        filestore_snapshots_dir: Option<std::path::PathBuf>,
    ) -> Result<Snapshot, CoreError> {
        let started = std::time::Instant::now();
        let source = self.get_database(database_id)?;
        let server = self.get_server(source.server_id)?;
        let name = name.into();
        let conn = self.pg_conn_info_for_running_instance(server.postgres_instance_id)?;

        let snapshot_database_name = Self::derive_snapshot_database_name(&source.name);
        pg_admin::duplicate_database(&conn, &source.name, &snapshot_database_name).await?;

        // When the caller says nothing, take the *whole* database — files
        // included. A caller that forgets an optional argument must not
        // silently get half a snapshot; see this section's comment above.
        // A database with no attachments yet simply has no directory, and
        // that is a normal state, not a failure.
        let (filestore_source, filestore_snapshots_dir) = match (filestore_source, filestore_snapshots_dir) {
            (None, None) => {
                let derived = self.database_filestore_path(database_id)?.filter(|p| p.is_dir());
                let dest_root = if derived.is_some() { self.snapshots_dir()? } else { None };
                (derived, dest_root)
            }
            explicit => explicit,
        };

        let filestore_snapshot_path = match (filestore_source, filestore_snapshots_dir) {
            (Some(src), Some(dest_root)) => {
                let dest = dest_root.join(&snapshot_database_name);
                let dest_for_task = dest.clone();
                tokio::task::spawn_blocking(move || filestore::snapshot_directory(&src, &dest_for_task))
                    .await
                    .expect("filestore snapshot task panicked")?;
                Some(dest.to_string_lossy().to_string())
            }
            _ => None,
        };

        // The branch this was taken on, recorded rather than left to be
        // inferred from the label later. People rename snapshots, and a
        // name that merely looks like a branch proves nothing — this is
        // what lets the app say "this snapshot is from the branch you're
        // on" as a fact instead of a guess.
        let snapshot = Snapshot::new(
            server.id,
            database_id,
            source.name.clone(),
            snapshot_database_name,
            name,
            filestore_snapshot_path,
        )
        .on_branch(self.current_git_branch(server.id).unwrap_or(None));
        self.db.insert_snapshot(&snapshot)?;
        self.record_timed_event(Event::SnapshotCreated {
            snapshot_id: snapshot.id,
            database_id,
            name: snapshot.name.clone(),
            has_filestore: snapshot.filestore_snapshot_path.is_some(),
        }, started.elapsed())?;
        Ok(snapshot)
    }

    pub fn list_snapshots(&self, database_id: Option<Uuid>) -> Result<Vec<Snapshot>, CoreError> {
        self.db.list_snapshots(database_id)
    }

    pub fn get_snapshot(&self, id: Uuid) -> Result<Snapshot, CoreError> {
        self.db.get_snapshot(id)?.ok_or_else(|| CoreError::NotFound(format!("snapshot {id}")))
    }

    /// Drops the snapshot's real Postgres database and, if it has one,
    /// removes its filestore copy — then removes the metadata row. Reaches
    /// Postgres via the snapshot's own `server_id`, not through
    /// `get_database(snapshot.database_id)`, precisely so a snapshot whose
    /// source database was already dropped is still cleanly deletable (see
    /// `Snapshot`'s doc comment).
    pub async fn delete_snapshot(&self, id: Uuid) -> Result<(), CoreError> {
        let snapshot = self.get_snapshot(id)?;
        let server = self.get_server(snapshot.server_id)?;
        let conn = self.pg_conn_info_for_running_instance(server.postgres_instance_id)?;
        pg_admin::drop_database(&conn, &snapshot.snapshot_database_name).await?;

        if let Some(path) = &snapshot.filestore_snapshot_path {
            let path = std::path::PathBuf::from(path);
            tokio::task::spawn_blocking(move || filestore::remove_snapshot_directory(&path))
                .await
                .expect("filestore removal task panicked")?;
        }

        self.db.delete_snapshot(id)?;
        self.record_event(Event::SnapshotDeleted { snapshot_id: id })?;
        Ok(())
    }

    // --- revert to snapshot (task 3.3) --------------------------------------
    //
    // "One-click revert" — restoring a server's *current* database (and,
    // when asked, a filestore directory) from an existing snapshot, in
    // place. Worth being precise about how this differs from two
    // similar-sounding operations already in this file: `create_snapshot`/
    // `delete_snapshot` (3.1) only ever create or remove a frozen side copy
    // and never touch a server's live database; `restore_database` (2.3)
    // replays a `pg_dump` file into a brand-new database and never
    // overwrites an existing one. This is the one operation that actually
    // overwrites a live database's contents in place — which is exactly why
    // it's rung 3 ("Restore snapshot over live DB") on the friction ladder
    // in `odoo-orchestrator-ui-design-principles.md`, the highest rung
    // short of a type-to-confirm delete, and why the ladder calls for a
    // counter-snapshot to be offered before it runs.

    /// Reverts `database_id`'s real Postgres database in place to
    /// `snapshot_id`'s frozen copy: drop the live database, then
    /// `CREATE DATABASE ... TEMPLATE` straight from the snapshot's own
    /// database — reusing `duplicate_database`'s mechanism a second time
    /// (3.1 already reused it once from 2.3), just with the direction
    /// reversed (snapshot → live, not live → snapshot). The `Database` row
    /// itself is untouched (same id, same name) — only what Postgres holds
    /// under that name changes.
    ///
    /// When `counter_snapshot_name` is `Some`, a real snapshot of the live
    /// database is taken (via `create_snapshot`, not reimplemented) *before*
    /// the destructive drop, so the caller gets back exactly what this
    /// revert can itself be undone with — the friction ladder's
    /// "counter-snapshot offered" element, proven for real rather than a
    /// UI-only promise. That counter-snapshot is deliberately database-only
    /// (no filestore half): there's no real Odoo filestore in this project
    /// to snapshot as a matter of course today (see `Snapshot`'s doc
    /// comment), so — same posture as `create_snapshot`'s own filestore
    /// parameters — nothing here pretends to capture a filestore it wasn't
    /// asked to.
    ///
    /// When `filestore_target` is given, the directory currently at that
    /// path is removed and replaced with a fresh hardlink-copy of the
    /// snapshot's own filestore copy (`filestore::snapshot_directory`,
    /// reused again rather than reimplemented) — this fails with
    /// `SnapshotHasNoFilestore` if the snapshot itself has no filestore half
    /// to revert from, rather than silently leaving the target untouched.
    pub async fn revert_database_to_snapshot(
        &self,
        database_id: Uuid,
        snapshot_id: Uuid,
        counter_snapshot_name: Option<String>,
        filestore_target: Option<std::path::PathBuf>,
    ) -> Result<Option<Snapshot>, CoreError> {
        let started = std::time::Instant::now();
        let database = self.get_database(database_id)?;
        let snapshot = self.get_snapshot(snapshot_id)?;
        if snapshot.database_id != database_id {
            return Err(CoreError::SnapshotNotForDatabase { snapshot_id, database_id });
        }
        if filestore_target.is_some() && snapshot.filestore_snapshot_path.is_none() {
            return Err(CoreError::SnapshotHasNoFilestore { snapshot_id });
        }
        // Mirror of `create_snapshot`: restore the whole database, files
        // included, unless the caller named a target itself. A snapshot
        // taken with attachments must never be restored without them —
        // that would put the rows back pointing at today's files.
        let filestore_target = match filestore_target {
            Some(explicit) => Some(explicit),
            None if snapshot.filestore_snapshot_path.is_some() => self.database_filestore_path(database_id)?,
            None => None,
        };
        let server = self.get_server(database.server_id)?;
        let conn = self.pg_conn_info_for_running_instance(server.postgres_instance_id)?;

        // Take the counter-snapshot before touching anything destructive —
        // if this fails, the live database is never dropped.
        let counter_snapshot = match counter_snapshot_name {
            Some(name) => Some(self.create_snapshot(database_id, name, None, None).await?),
            None => None,
        };

        pg_admin::drop_database(&conn, &database.name).await?;
        pg_admin::duplicate_database(&conn, &snapshot.snapshot_database_name, &database.name).await?;

        if let Some(filestore_target) = filestore_target {
            // Validated above: filestore_snapshot_path is Some whenever
            // filestore_target is Some.
            let src = std::path::PathBuf::from(snapshot.filestore_snapshot_path.clone().unwrap());
            tokio::task::spawn_blocking(move || {
                if filestore_target.exists() {
                    filestore::remove_snapshot_directory(&filestore_target)?;
                }
                filestore::snapshot_directory(&src, &filestore_target)
            })
            .await
            .expect("filestore revert task panicked")?;
        }

        self.record_timed_event(Event::DatabaseRevertedToSnapshot {
            database_id,
            snapshot_id,
            counter_snapshot_id: counter_snapshot.as_ref().map(|s| s.id),
        }, started.elapsed())?;

        Ok(counter_snapshot)
    }

    // --- git-branch binding (task 3.2) --------------------------------------
    //
    // "Associate a snapshot with the repo's current branch automatically
    // when one is linked." There's no separate "linked repo" concept in
    // this codebase to invent — a server's `Private`-kind addons source
    // (see `create_addons_source`'s own test, which literally labels one
    // "this repo") already *is* the user's own working copy, the one
    // that's actually under active development and branched day to day;
    // `Oca`/`Core` sources are vendor code a user doesn't typically branch.
    // So this reuses that existing registration rather than adding a new
    // per-server "repo path" field that would just duplicate it.

    /// A best-effort suggestion for a snapshot's name: the real current
    /// branch of the server's first registered `Private` addons source, if
    /// there is one and it's genuinely a git working directory sitting on a
    /// resolvable branch. Returns `Ok(None)` — not an error — for every
    /// case where there's honestly nothing to suggest (no private addons
    /// source registered yet, its path isn't a git repo, it has no commits,
    /// or it's on a detached HEAD): a snapshot's `name` is free-form text
    /// either way (see `Snapshot`'s own doc comment), so a caller can
    /// always fall back to asking the user directly, exactly like this
    /// project already does when 2.2's Odoo checkout or 0.6/0.7's desktop
    /// environment aren't available — a missing prerequisite here is a
    /// normal, handled case, not a hard failure.
    pub fn current_git_branch(&self, server_id: Uuid) -> Result<Option<String>, CoreError> {
        self.get_server(server_id)?;
        let sources = self.db.list_addons_sources(server_id)?;
        let Some(private_source) = sources.iter().find(|s| s.kind == SourceKind::Private) else {
            return Ok(None);
        };
        match git::current_branch(std::path::Path::new(&private_source.path_or_url)) {
            Ok(branch) => Ok(Some(branch)),
            Err(_) => Ok(None),
        }
    }

    // --- reloading on save --------------------------------------------------
    //
    // Two mechanisms, deliberately split by who does them best:
    //
    // **Python changes are Odoo's job.** `--dev=reload` watches `.py`
    // files, compiles each change before acting — so a syntax error is
    // logged instead of restarting into a broken state — and restarts the
    // process itself. Reimplementing that here would be strictly worse
    // than the thing Odoo already ships.
    //
    // **Data changes are nobody's job, which is why they're ours.** A
    // change to a data XML or CSV file only takes effect after `-u
    // <module>`, and nothing in Odoo watches for that. This is the gap
    // worth filling.

    /// Turns reloading on or off for one Odoo, and makes it true rather
    /// than merely recorded.
    ///
    /// Enabling `reload_python` **installs `watchdog` into that Odoo's
    /// interpreter**, because `--dev=reload` silently does nothing without
    /// it — Odoo logs "Code autoreload feature is disabled" and carries on.
    /// A setting that quietly does nothing is worse than no setting, so
    /// this refuses to be one.
    ///
    /// Takes effect on the Odoo's next start: `odoo.conf` is written at
    /// start time, and rewriting it under a running process would change
    /// what the file says without changing what the process does.
    pub async fn set_reload_policy(&self, server_id: Uuid, policy: model::ReloadPolicy) -> Result<OdooServer, CoreError> {
        let server = self.get_server(server_id)?;
        if policy.reload_python {
            let runtime = self.ensure_odoo(&server.odoo_version).await?;
            if let Some(python) = runtime.venv_python.clone() {
                let config = python_runtime::PythonRuntimeConfig {
                    uv_bin: discover_uv_bin()?,
                    install_dir: self.app_dir()?.join("python"),
                };
                python_runtime::install_requirements(&config, &python, &["watchdog".to_string()]).await?;
            }
        }
        self.db.set_reload_policy(server_id, policy)?;
        self.restart_data_watcher(server_id)?;
        self.get_server(server_id)
    }

    /// Starts, restarts or stops the data-file watcher for one Odoo, to
    /// match its current policy. Idempotent — safe to call whenever
    /// anything that affects it changes.
    fn restart_data_watcher(&self, server_id: Uuid) -> Result<(), CoreError> {
        // Dropping the old watcher stops it: `notify`'s watcher lives as
        // long as its handle.
        self.data_watchers.lock().expect("data watchers mutex poisoned").remove(&server_id);

        let server = self.get_server(server_id)?;
        if !server.reload.update_on_data_change {
            return Ok(());
        }
        let folders: Vec<std::path::PathBuf> = self
            .db
            .list_addons_sources(server_id)?
            .into_iter()
            // Only the user's own code. Watching Odoo's own 40,000 files
            // for edits nobody is making is pure cost.
            .filter(|s| s.kind != SourceKind::Core)
            .map(|s| std::path::PathBuf::from(s.path_or_url))
            .filter(|p| p.is_dir())
            .collect();
        if folders.is_empty() {
            return Ok(());
        }

        let core = self.clone();
        let (tx, rx) = std::sync::mpsc::channel::<notify::Result<notify::Event>>();
        let mut watcher = match notify::recommended_watcher(tx) {
            Ok(watcher) => watcher,
            Err(err) => {
                tracing::warn!("couldn't watch for data changes: {err}");
                return Ok(());
            }
        };
        for folder in &folders {
            use notify::Watcher;
            if let Err(err) = watcher.watch(folder, notify::RecursiveMode::Recursive) {
                tracing::warn!("couldn't watch {}: {err}", folder.display());
            }
        }

        let roots = folders.clone();
        std::thread::spawn(move || {
            // Debounced: an editor writing one file produces several
            // events, and a `git checkout` produces hundreds. Updating a
            // module once per burst is the difference between a useful
            // feature and one that hammers a database.
            let quiet = std::time::Duration::from_millis(1200);
            let mut pending: std::collections::HashSet<String> = std::collections::HashSet::new();
            loop {
                let first = match rx.recv() {
                    Ok(event) => event,
                    Err(_) => return, // the watcher was dropped; so is this thread's job
                };
                collect_modules(&first, &roots, &mut pending);
                // Drain whatever else arrives during the quiet period.
                while let Ok(event) = rx.recv_timeout(quiet) {
                    collect_modules(&event, &roots, &mut pending);
                }
                if pending.is_empty() {
                    continue;
                }
                let modules: Vec<String> = pending.drain().collect();
                let core = core.clone();
                // The update is real work against a real database, so it
                // runs on the async runtime rather than blocking the
                // watcher thread that has to keep draining events.
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    handle.spawn(async move { core.update_after_data_change(server_id, modules).await });
                }
            }
        });

        self.data_watchers.lock().expect("data watchers mutex poisoned").insert(server_id, watcher);
        Ok(())
    }

    /// Runs `-u` for modules whose data files changed, on every database
    /// of this Odoo that actually has them installed.
    ///
    /// Deliberately skips databases where the module isn't installed:
    /// `-u` on an uninstalled module is a no-op that still pays the whole
    /// cost of booting Odoo's registry.
    async fn update_after_data_change(&self, server_id: Uuid, modules: Vec<String>) {
        let Ok(databases) = self.list_databases(Some(server_id)) else { return };
        for database in databases {
            let installed: Vec<String> = match self.database_module_states(database.id).await {
                Ok(states) => states
                    .into_iter()
                    .filter(|s| s.state == "installed" && modules.contains(&s.technical_name))
                    .map(|s| s.technical_name)
                    .collect(),
                Err(_) => continue,
            };
            if installed.is_empty() {
                continue;
            }
            tracing::info!("data files changed, updating {:?} on {}", installed, database.name);
            if let Err(err) = self.upgrade_modules_batched(database.id, &installed).await {
                tracing::warn!("couldn't update {:?} on {}: {err}", installed, database.name);
            }
        }
    }

    /// The snapshots taken on `branch` for any database on this Odoo,
    /// newest first.
    ///
    /// This is what makes branch binding useful rather than decorative:
    /// switch to a branch, and the app can offer the database state that
    /// belonged to it — instead of leaving you to remember which of
    /// fourteen snapshots went with which piece of work.
    pub fn snapshots_on_branch(&self, server_id: Uuid, branch: &str) -> Result<Vec<Snapshot>, CoreError> {
        let mut found: Vec<Snapshot> = self
            .db
            .list_snapshots(None)?
            .into_iter()
            .filter(|s| s.server_id == server_id && s.git_branch.as_deref() == Some(branch))
            .collect();
        found.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(found)
    }

    // --- Odoo server supervision (task 2.2 completion) ---------------------
    //
    // Wires `odoo::OdooSupervisor` (spawn/watch/stop a real process) and
    // `odoo_conf` (render a real config file) together against a server's
    // registered addons sources and its assigned, running `PostgresInstance`.
    // Deliberately does NOT provision the Python venv itself — that's a
    // slow, network-touching, explicit precondition the caller satisfies
    // once via `python_runtime::{ensure_python_installed, ensure_venv,
    // install_requirements}` (same "explicit, not magic" posture as
    // `create_snapshot`'s explicit filestore params), not something bundled
    // into every `start_server` call.

    /// Get (or lazily construct) this server's live `OdooSupervisor`. Like
    /// `get_or_create_supervisor` for Postgres, the `config` passed here
    /// only takes effect on first construction — a server already running
    /// (or previously started this process run) reuses its existing
    /// supervisor and ignores a differing `config` on a later call.
    /// Keeps one line of real process output: into the ring buffer for
    /// anyone who opens the Logs tab later, and onto the channel for
    /// anyone watching right now.
    fn record_log_line(&self, server_id: Uuid, stream: &str, line: String) {
        let entry = LogLine { server_id, at: chrono::Utc::now(), stream: stream.to_string(), line };
        {
            let mut buffers = self.log_buffers.lock().expect("log buffers mutex poisoned");
            let buffer = buffers.entry(server_id).or_default();
            if buffer.len() >= LOG_BUFFER_LINES {
                buffer.pop_front();
            }
            buffer.push_back(entry.clone());
        }
        self.log_bus.publish(entry);
    }

    /// The most recent output from one Odoo, oldest first — up to `limit`
    /// lines, or everything kept if `limit` is larger than the buffer.
    ///
    /// Empty is a real answer: an Odoo that hasn't been started during
    /// *this run of the app* has no output here, because the buffer lives
    /// in memory and does not survive a restart. The UI says so rather
    /// than letting an empty pane read as "nothing went wrong".
    pub fn recent_logs(&self, server_id: Uuid, limit: usize) -> Vec<LogLine> {
        let buffers = self.log_buffers.lock().expect("log buffers mutex poisoned");
        let Some(buffer) = buffers.get(&server_id) else { return Vec::new() };
        let skip = buffer.len().saturating_sub(limit);
        buffer.iter().skip(skip).cloned().collect()
    }

    /// Live process output, as it happens.
    pub fn subscribe_logs(&self) -> tokio::sync::broadcast::Receiver<LogLine> {
        self.log_bus.subscribe()
    }

    /// Real memory and CPU for every Odoo currently running, keyed by
    /// server id. Measured from the OS at call time.
    ///
    /// An Odoo that isn't running simply isn't in the map — the Stats
    /// screen shows what's up, and a stopped process has no usage to
    /// report rather than zero.
    pub fn running_odoo_usage(&self) -> HashMap<Uuid, odoo::ProcessUsage> {
        let supervisors: Vec<(Uuid, odoo::OdooSupervisor)> = {
            let map = self.odoo_supervisors.lock().expect("odoo supervisors mutex poisoned");
            map.iter().map(|(id, sup)| (*id, sup.clone())).collect()
        };
        supervisors.into_iter().filter_map(|(id, sup)| sup.usage().map(|usage| (id, usage))).collect()
    }

    fn get_or_create_odoo_supervisor(&self, server_id: Uuid, config: odoo::OdooConfig) -> odoo::OdooSupervisor {
        {
            let map = self.odoo_supervisors.lock().expect("odoo supervisors mutex poisoned");
            if let Some(sup) = map.get(&server_id) {
                return sup.clone();
            }
        }

        let core = self.clone();
        let sup = odoo::OdooSupervisor::with_listener(config, move |notification| {
            // Real process output goes to the bounded per-server buffer
            // and the log channel — not the event log, which stays the
            // durable record of what the *app* did.
            let state = match notification {
                odoo::OdooNotification::Log { stream, line } => {
                    core.record_log_line(server_id, stream_name(stream), line);
                    return;
                }
                odoo::OdooNotification::State(state) => state,
            };
            let server_state = match state {
                odoo::OdooState::Stopped => ServerState::Stopped,
                odoo::OdooState::Starting => ServerState::Starting,
                odoo::OdooState::Running { .. } => ServerState::Running,
                odoo::OdooState::Stopping => ServerState::Stopping,
                odoo::OdooState::Crashed { exit_code } => ServerState::Crashed { exit_code: exit_code.unwrap_or(-1) },
            };
            // Best-effort, same posture as the Postgres listener above —
            // this closure can't return a `Result` to any caller.
            let _ = core.db.update_server_state(server_id, &server_state);
            let event = match state {
                odoo::OdooState::Running { .. } => Some(Event::ServerStarted { server_id }),
                odoo::OdooState::Stopped => Some(Event::ServerStopped { server_id }),
                odoo::OdooState::Crashed { exit_code } => Some(Event::ServerCrashed { server_id, exit_code: exit_code.unwrap_or(-1) }),
                _ => None,
            };
            if let Some(event) = event {
                let _ = core.record_event(event);
            }
        });

        let mut map = self.odoo_supervisors.lock().expect("odoo supervisors mutex poisoned");
        map.entry(server_id).or_insert(sup).clone()
    }

    /// Renders a real `odoo.conf` under `runtime_dir/<server_id>/` (creating
    /// its own `filestore` subdirectory) from this server's registered
    /// addons sources (in `rank` order) and its assigned `PostgresInstance`
    /// — which must already be `Running` (same precondition every other
    /// Postgres-touching `Core` method has) — then spawns real `odoo-bin`
    /// from `checkout_root` under `venv_python` and returns once it's
    /// actually serving HTTP on `server.port`, exactly like
    /// `start_postgres_instance` does for `pg_ctl`.
    /// Start an Odoo, working out for itself which copy of Odoo to run
    /// and where to put its generated config — the caller supplies
    /// nothing but the id. Fetches the version if this machine doesn't
    /// have it yet.
    pub async fn start_server_here(&self, server_id: Uuid) -> Result<(), CoreError> {
        let (checkout_root, python, run_dir) = self.resolve_runtime_for(server_id).await?;
        self.start_server(server_id, checkout_root, python, run_dir).await
    }

    /// Where this Odoo's code, interpreter and working directory are —
    /// found, or fetched if this machine hasn't got that version yet.
    ///
    /// Exists so nothing outside `Core` has to know these paths. A UI that
    /// had to supply them could only guess, which is exactly how the
    /// module install/upgrade buttons ended up unable to work at all: the
    /// API demanded three paths the browser has no way to know, so every
    /// call failed on a missing field.
    async fn resolve_runtime_for(
        &self,
        server_id: Uuid,
    ) -> Result<(std::path::PathBuf, std::path::PathBuf, std::path::PathBuf), CoreError> {
        let server = self.get_server(server_id)?;
        let runtime = self.ensure_odoo(&server.odoo_version).await?;
        let python = match runtime.venv_python.clone() {
            Some(python) => python,
            // A checkout with no interpreter beside it is the normal state
            // for one this app adopted from the machine — somebody's
            // `~/odoo-dev` is source, not an environment. Provisioning it
            // is the app's job; refusing to start until the user builds a
            // venv by hand would be exactly the "go and set it up
            // yourself" this product exists to remove.
            None => self.ensure_python_for(&runtime).await?,
        };
        Ok((runtime.checkout_root, python, self.app_dir()?.join("run")))
    }

    /// Builds this Odoo an interpreter with its own `requirements.txt`
    /// installed, under the app's own folder — one per version, shared by
    /// every project on it, the same way the checkout itself is.
    ///
    /// Kept out of the checkout deliberately: a checkout adopted from the
    /// user's machine belongs to them, and this app has no business
    /// writing a `.venv` into somebody's working repository.
    async fn ensure_python_for(&self, runtime: &odoo_runtime::OdooRuntime) -> Result<std::path::PathBuf, CoreError> {
        let config = python_runtime::PythonRuntimeConfig {
            uv_bin: discover_uv_bin()?,
            install_dir: self.app_dir()?.join("python"),
        };
        let venv_dir = self.app_dir()?.join("venvs").join(&runtime.version);
        let existing = venv_dir.join("bin").join("python");
        if existing.is_file() {
            return Ok(existing);
        }

        // Odoo 17 wants 3.11; older series want older interpreters, but
        // this app only offers versions it can actually run.
        python_runtime::ensure_python_installed(&config, "3.11").await?;
        let venv_python = python_runtime::ensure_venv(&config, "3.11", &venv_dir).await?;

        let requirements_txt = std::fs::read_to_string(runtime.checkout_root.join("requirements.txt"))?;
        // Odoo's own requirements.txt trails environment-marker lines with
        // a `# comment` explaining the pin. `uv` parses `;` markers but not
        // a trailing `#` on a bare CLI specifier — that's requirements-file
        // syntax, not specifier syntax.
        let requirements: Vec<String> = requirements_txt
            .lines()
            .map(|line| line.split('#').next().unwrap_or("").trim().to_string())
            .filter(|line| !line.is_empty())
            .collect();
        python_runtime::install_requirements(&config, &venv_python, &requirements).await?;
        // `odoo/modules/module.py` imports `pkg_resources`, which
        // setuptools 81+ no longer ships. requirements.txt doesn't pin
        // setuptools, so without this the venv resolves the latest one and
        // real `odoo-bin` dies at import time.
        python_runtime::install_requirements(&config, &venv_python, &["setuptools<81".to_string()]).await?;
        Ok(venv_python)
    }

    /// Install modules without being told where anything is — the same
    /// "the app knows its own layout" posture as `start_server_here`.
    pub async fn install_modules_here(&self, database_id: Uuid, module_names: &[String]) -> Result<(), CoreError> {
        let database = self.get_database(database_id)?;
        let (root, python, run_dir) = self.resolve_runtime_for(database.server_id).await?;
        self.install_modules(database_id, module_names, root, python, run_dir).await
    }

    /// Updates several modules in **one** Odoo boot, falling back to one at
    /// a time only when that fails.
    ///
    /// Odoo's `-u a,b,c` loads the registry once; running `-u` per module
    /// pays that cost per module, and the registry load is nearly all of
    /// the time. The old loop did it per module purely so a failure could
    /// name the module that failed — which is worth having, but not worth
    /// paying for on every success. So: batch first, and if the batch
    /// fails, retry singly to find out which one broke.
    pub async fn upgrade_modules_batched(&self, database_id: Uuid, module_names: &[String]) -> Result<(), CoreError> {
        if module_names.len() <= 1 {
            return self.upgrade_modules_here(database_id, module_names).await;
        }
        match self.upgrade_modules_here(database_id, module_names).await {
            Ok(()) => Ok(()),
            Err(batch_error) => {
                tracing::warn!("batched update of {module_names:?} failed; retrying one at a time to find the culprit");
                for name in module_names {
                    self.upgrade_modules_here(database_id, std::slice::from_ref(name)).await?;
                }
                // Every module succeeded alone, so the batch failed for a
                // reason none of them owns — hand back the original error
                // rather than pretending it went fine.
                Err(batch_error)
            }
        }
    }

    pub async fn upgrade_modules_here(&self, database_id: Uuid, module_names: &[String]) -> Result<(), CoreError> {
        let database = self.get_database(database_id)?;
        let (root, python, run_dir) = self.resolve_runtime_for(database.server_id).await?;
        self.upgrade_modules(database_id, module_names, root, python, run_dir).await
    }

    pub async fn uninstall_modules_here(&self, database_id: Uuid, module_names: &[String]) -> Result<(), CoreError> {
        let database = self.get_database(database_id)?;
        let (root, python, run_dir) = self.resolve_runtime_for(database.server_id).await?;
        self.uninstall_modules(database_id, module_names, root, python, run_dir).await
    }

    /// The `odoo.conf` this server would be started with right now.
    ///
    /// Rendered by the **same** `odoo_conf::render` that `start_server`
    /// writes to disk, from the same inputs, so the config shown in the UI
    /// cannot drift away from the config that actually runs. A hand-written
    /// approximation in the front end is how a "look, no black box" panel
    /// quietly becomes a lie; this is the fix for that whole class.
    ///
    /// Works whether or not Postgres is running and whether or not this
    /// version of Odoo has been downloaded — it is a preview, not a
    /// launch.
    pub fn preview_odoo_conf(&self, server_id: Uuid) -> Result<StartupPreview, CoreError> {
        let server = self.get_server(server_id)?;
        let mut sources = self.db.list_addons_sources(server_id)?;
        sources.sort_by_key(|s| s.rank);
        let addons_path: Vec<String> = sources.into_iter().map(|s| s.path_or_url).collect();

        let instance = self.get_postgres_instance(server.postgres_instance_id)?;
        let filestore_dir = match self.data_dir.lock().expect("data dir mutex poisoned").clone() {
            Some(data_dir) => data_dir.join("run").join(server_id.to_string()).join("filestore"),
            None => std::path::PathBuf::from("(no data folder configured yet)"),
        };

        let conf = odoo_conf::render(&odoo_conf::OdooConfParams {
            addons_path,
            db_host: std::path::Path::new(&instance.data_dir),
            db_port: instance.port,
            // Odoo gets its own non-superuser role, created on first start —
            // see the comment in `start_server`.
            db_user: "odoo",
            http_port: server.port,
            data_dir: &filestore_dir,
        });

        // The flags matter as much as the file — auto-reload lives here and
        // nowhere else — so a panel that shows only the config file would
        // still be hiding half of what runs.
        let mut command = vec!["odoo-bin".to_string(), "-c".to_string(), "odoo.conf".to_string()];
        if server.reload.reload_python {
            command.push("--dev=reload,qweb,xml".to_string());
        }
        Ok(StartupPreview { conf, command: command.join(" ") })
    }

    pub async fn start_server(
        &self,
        server_id: Uuid,
        checkout_root: impl Into<std::path::PathBuf>,
        venv_python: impl Into<std::path::PathBuf>,
        runtime_dir: impl Into<std::path::PathBuf>,
    ) -> Result<(), CoreError> {
        let server = self.get_server(server_id)?;
        let mut sources = self.db.list_addons_sources(server_id)?;
        sources.sort_by_key(|s| s.rank);
        let addons_path: Vec<String> = sources.into_iter().map(|s| s.path_or_url).collect();

        let admin_conn = self.pg_conn_info_for_running_instance(server.postgres_instance_id)?;
        // Real Odoo hard-refuses to start when its own `db_user` is the
        // cluster's superuser role (see `pg_admin::ensure_role`'s doc
        // comment) — which is exactly the role every other Postgres-touching
        // `Core` method above connects as. So Odoo gets its own, separate,
        // non-superuser role here, created idempotently on first use.
        pg_admin::ensure_role(&admin_conn, "odoo").await?;
        let odoo_conn = pg_admin::PgConnInfo { user: "odoo".to_string(), ..admin_conn };

        let checkout_root = checkout_root.into();
        let server_runtime_dir = runtime_dir.into().join(server_id.to_string());
        let filestore_dir = server_runtime_dir.join("filestore");
        std::fs::create_dir_all(&filestore_dir)?;
        // Remembered so `database_filestore_path` finds the real files
        // even when this was called with an explicit `runtime_dir`.
        self.filestore_dirs
            .lock()
            .expect("filestore dirs mutex poisoned")
            .insert(server_id, filestore_dir.clone());

        let conf = odoo_conf::render(&odoo_conf::OdooConfParams {
            addons_path,
            db_host: &odoo_conn.host,
            db_port: odoo_conn.port,
            db_user: &odoo_conn.user,
            http_port: server.port,
            data_dir: &filestore_dir,
        });
        let conf_path = server_runtime_dir.join("odoo.conf");
        std::fs::write(&conf_path, conf)?;

        let odoo_bin = checkout_root.join("odoo-bin");
        let mut argv = vec![odoo_bin.display().to_string(), "-c".to_string(), conf_path.display().to_string()];
        // On the command line, not in `odoo.conf`. Odoo's own `_parse_config`
        // recomputes `dev_mode` from the `--dev` option *after* reading the
        // config file, and lists `dev_mode` as "not exposed in the
        // configuration file" — so a `dev_mode =` line there is read and
        // discarded, silently. See `odoo_conf`'s module doc comment for the
        // real-Odoo run that proved it.
        if server.reload.reload_python {
            argv.push("--dev=reload,qweb,xml".to_string());
        }
        let mut odoo_config = odoo::OdooConfig::new(
            venv_python.into(),
            checkout_root,
            argv,
            server.port,
        );
        // Real Odoo initializing its ORM/registry genuinely takes longer
        // than the stand-in HTTP server `odoo.rs`'s own tests use.
        odoo_config.startup_timeout = std::time::Duration::from_secs(120);

        let sup = self.get_or_create_odoo_supervisor(server_id, odoo_config);
        sup.start().await?;
        Ok(())
    }

    /// Stops this server's real `odoo-bin` process, if one is running (or
    /// was ever started this process run) — a harmless no-op otherwise,
    /// same posture as `OdooSupervisor::stop` itself and `stop_postgres_instance`.
    pub async fn stop_server(&self, server_id: Uuid) -> Result<(), CoreError> {
        self.get_server(server_id)?;
        let sup = {
            let map = self.odoo_supervisors.lock().expect("odoo supervisors mutex poisoned");
            map.get(&server_id).cloned()
        };
        if let Some(sup) = sup {
            sup.stop().await?;
        }
        Ok(())
    }

    // --- module install/upgrade/uninstall (task 2.4) ------------------------
    //
    // Odoo's own `ir_module_module` state is per-database even though the
    // addons-path (what's *available* to install) is per-server — so these
    // take a `database_id`, not a `server_id`. Install/upgrade run one-shot
    // `odoo-bin -i`/`-u ... --stop-after-init` (Odoo has no separate
    // "initialize this database" step: running `-i` against a bare
    // Postgres database bootstraps the whole framework, since every module
    // depends on `base`); uninstall has no CLI flag at all, so it goes
    // through `odoo-bin shell` running a real `ir.module.module` ORM call
    // instead. Neither needs `OdooSupervisor`'s long-running state machine
    // — see `odoo::run_one_shot`'s doc comment. Deliberately explicit-params,
    // same posture as `start_server`: doesn't provision a venv or infer a
    // checkout root either.

    /// Shared preamble for every module command below: resolve the
    /// database's server and running Postgres instance, ensure the
    /// non-superuser `odoo` role exists (same requirement `start_server`
    /// has), and render this server's `odoo.conf` — reusing the exact same
    /// path `start_server` writes to, so running a module command while the
    /// server also happens to be running doesn't leave two different
    /// configs for the same server disagreeing with each other. Returns the
    /// admin (superuser) connection too, for `upgrade_modules`' own
    /// after-the-fact `ir_module_module` read.
    async fn prepare_odoo_runtime_for_database(
        &self,
        database_id: Uuid,
        checkout_root: impl Into<std::path::PathBuf>,
        runtime_dir: impl Into<std::path::PathBuf>,
    ) -> Result<(Database, std::path::PathBuf, std::path::PathBuf, pg_admin::PgConnInfo), CoreError> {
        let database = self.get_database(database_id)?;
        let server = self.get_server(database.server_id)?;
        let mut sources = self.db.list_addons_sources(server.id)?;
        sources.sort_by_key(|s| s.rank);
        let addons_path: Vec<String> = sources.into_iter().map(|s| s.path_or_url).collect();

        let admin_conn = self.pg_conn_info_for_running_instance(server.postgres_instance_id)?;
        pg_admin::ensure_role(&admin_conn, "odoo").await?;
        // Postgres 15+ revoked every non-owner role's default CREATE
        // privilege on a fresh database's own `public` schema — without
        // this, `odoo` can log in fine but fails the instant it tries to
        // create its first table (see `grant_schema_privileges`'s doc
        // comment). Scoped per-database, so this runs here (not just once
        // in `ensure_role`) — every database this touches needs it granted
        // on itself.
        pg_admin::grant_schema_privileges(&admin_conn, &database.name, "odoo").await?;
        let odoo_conn = pg_admin::PgConnInfo { user: "odoo".to_string(), ..admin_conn.clone() };

        let checkout_root = checkout_root.into();
        let server_runtime_dir = runtime_dir.into().join(server.id.to_string());
        let filestore_dir = server_runtime_dir.join("filestore");
        std::fs::create_dir_all(&filestore_dir)?;

        let conf = odoo_conf::render(&odoo_conf::OdooConfParams {
            addons_path,
            db_host: &odoo_conn.host,
            db_port: odoo_conn.port,
            db_user: &odoo_conn.user,
            http_port: server.port,
            data_dir: &filestore_dir,
            // Deliberately never dev mode: this config is for a one-shot
            // `-i`/`-u` process that runs and exits. An autoreloader
            // inside it would watch for changes to a server that is about
            // to stop existing.
        });
        let conf_path = server_runtime_dir.join("odoo.conf");
        std::fs::write(&conf_path, conf)?;

        Ok((database, checkout_root, conf_path, admin_conn))
    }

    async fn run_module_flag_command(
        &self,
        database_id: Uuid,
        module_names: &[String],
        flag: &str,
        checkout_root: impl Into<std::path::PathBuf>,
        venv_python: impl Into<std::path::PathBuf>,
        runtime_dir: impl Into<std::path::PathBuf>,
    ) -> Result<(Database, pg_admin::PgConnInfo, odoo::OneShotOutput), CoreError> {
        let (database, checkout_root, conf_path, admin_conn) =
            self.prepare_odoo_runtime_for_database(database_id, checkout_root, runtime_dir).await?;
        let odoo_bin = checkout_root.join("odoo-bin");
        let args = vec![
            odoo_bin.display().to_string(),
            "-c".to_string(),
            conf_path.display().to_string(),
            "-d".to_string(),
            database.name.clone(),
            flag.to_string(),
            module_names.join(","),
            "--stop-after-init".to_string(),
            "--no-http".to_string(),
        ];
        let output = odoo::run_one_shot(&venv_python.into(), &checkout_root, &args, &[], None).await?;
        Ok((database, admin_conn, output))
    }

    // --- major-version migration ------------------------------------------
    //
    // The scariest thing an Odoo agency does, and the one with no tooling
    // at all. OCA's OpenUpgrade is the only community path, and it is
    // strictly one major version at a time — 15 → 18 is three separate
    // migrations, each an `odoo-bin --update all` against a database you
    // had better have copied first. Nobody has wrapped that chain. If hop
    // three fails you are between two versions with whatever you remembered
    // to back up.
    //
    // So the wrapper is the product: work on a **copy** so the original is
    // never at risk, snapshot before every hop so any hop can be undone,
    // stop at the first failure, and say exactly where you stopped.

    /// What migrating `database_id` to `target_version` would involve,
    /// without doing any of it.
    ///
    /// Every hop reports whether its pieces are already on disk, because
    /// "this will take ten minutes of downloading first" is something to
    /// learn before starting, not during.
    pub async fn plan_upgrade(&self, database_id: Uuid, target_version: &str) -> Result<UpgradePlan, CoreError> {
        let database = self.get_database(database_id)?;
        let server = self.get_server(database.server_id)?;
        let from = server.odoo_version.clone();

        let Some(chain) = openupgrade::hops(&from, target_version) else {
            return Err(CoreError::NotConfigured(format!(
                "there's no OpenUpgrade path from {from} to {target_version}"
            )));
        };

        let app_dir = self.app_dir()?;
        let mut hops = Vec::new();
        let mut previous = from.clone();
        for to in chain {
            hops.push(openupgrade::Hop {
                odoo_ready: self.find_odoo(&to)?.is_some(),
                openupgrade_ready: openupgrade::cache_dir(&app_dir, &to)
                    .join("openupgrade_framework")
                    .join("__manifest__.py")
                    .is_file(),
                from: std::mem::replace(&mut previous, to.clone()),
                to,
            });
        }

        Ok(UpgradePlan {
            database_id,
            database_name: database.name.clone(),
            from,
            to: target_version.to_string(),
            carried_addons: self.carried_addons_for(server.id)?,
            // The copy this would work on. The original database is never
            // touched by a migration — that is the whole reason somebody
            // would use this rather than running OpenUpgrade by hand.
            into: format!("{}_v{}", database.name, target_version.replace('.', "")),
            hops,
        })
    }

    /// Runs a planned migration, on a copy, checkpointing before each hop.
    ///
    /// Stops at the first hop that fails and reports which one. Everything
    /// before it stands, the checkpoint taken before it is a real snapshot,
    /// and the original database has not been touched at any point.
    pub async fn run_upgrade(&self, database_id: Uuid, target_version: &str) -> Result<UpgradeRun, CoreError> {
        let plan = self.plan_upgrade(database_id, target_version).await?;
        let started = std::time::Instant::now();
        let database = self.get_database(database_id)?;
        let server = self.get_server(database.server_id)?;

        if plan.hops.is_empty() {
            return Ok(UpgradeRun { plan, working_database_id: None, completed: Vec::new(), failed: None });
        }

        // The copy. From here on the original is out of the picture.
        let working = self.duplicate_database(database_id, plan.into.clone()).await?;
        self.record_event(Event::UpgradeStarted {
            database_id,
            working_database_id: working.id,
            from: plan.from.clone(),
            to: plan.to.clone(),
            hops: plan.hops.len() as u32,
        })?;

        let mut completed = Vec::new();
        // Cloned so the loop doesn't borrow the plan it has to hand back.
        let chain = plan.hops.clone();
        for hop in &chain {
            // The checkpoint, before anything. A hop that fails half way
            // through leaves a database that is neither version, and this
            // is the only way back from that.
            let checkpoint = self
                .create_snapshot(working.id, format!("before {} → {}", hop.from, hop.to), None, None)
                .await?;

            match self.run_one_hop(&working, hop, server.postgres_instance_id).await {
                Ok(()) => {
                    self.record_event(Event::UpgradeHopFinished {
                        working_database_id: working.id,
                        from: hop.from.clone(),
                        to: hop.to.clone(),
                    })?;
                    completed.push(hop.clone());
                }
                Err(err) => {
                    let message = err.to_string();
                    self.record_event(Event::UpgradeHopFailed {
                        working_database_id: working.id,
                        from: hop.from.clone(),
                        to: hop.to.clone(),
                        message: message.clone(),
                    })?;
                    return Ok(UpgradeRun {
                        plan,
                        working_database_id: Some(working.id),
                        completed,
                        failed: Some(FailedHop { hop: hop.clone(), message, checkpoint_id: checkpoint.id }),
                    });
                }
            }
        }

        self.record_timed_event(
            Event::UpgradeFinished { database_id, working_database_id: working.id, to: plan.to.clone() },
            started.elapsed(),
        )?;
        Ok(UpgradeRun { plan, working_database_id: Some(working.id), completed, failed: None })
    }

    /// One hop: the real `odoo-bin --update all` OpenUpgrade documents,
    /// with the pieces that hop needs fetched first.
    async fn run_one_hop(
        &self,
        working: &Database,
        hop: &openupgrade::Hop,
        postgres_instance_id: Uuid,
    ) -> Result<(), CoreError> {
        let app_dir = self.app_dir()?;
        let runtime = self.ensure_odoo(&hop.to).await?;
        let openupgrade_root = {
            let app_dir = app_dir.clone();
            let version = hop.to.clone();
            tokio::task::spawn_blocking(move || openupgrade::ensure(&app_dir, &version))
                .await
                .expect("openupgrade clone task panicked")?
        };

        let venv_python = self.ensure_python_for(&runtime).await?;
        // OpenUpgrade's own single dependency. Installed from git rather
        // than PyPI because its own documentation says to: the migration
        // helpers change faster than releases are cut.
        let config = python_runtime::PythonRuntimeConfig {
            uv_bin: discover_uv_bin()?,
            install_dir: app_dir.join("python"),
        };
        python_runtime::install_requirements(
            &config,
            &venv_python,
            &["openupgradelib @ git+https://github.com/OCA/openupgradelib.git@master".to_string()],
        )
        .await?;

        // The addons path for a migration is Odoo's own plus OpenUpgrade's
        // repository root, which is where its two modules live, **plus**
        // the server's own Private/OCA addons sources (never Core — this
        // hop's own checkout above already supplies that).
        //
        // Carrying custom code here is not optional politeness: without it,
        // a database that actually has a custom module installed cannot be
        // migrated at all. `--update all` reads every *installed* module
        // from `ir_module_module` and needs to resolve each one on the
        // addons path to run its `_auto_init` — a module Odoo can't find is
        // an outright failure, not a module that's merely skipped. That was
        // this project's top open product question (see
        // `odoo-orchestrator-whats-next.md`), and it was verified for real:
        // `a_migration_without_carried_addons_fails_on_a_custom_module`
        // reproduces the failure with the old behaviour, and
        // `a_custom_module_survives_a_real_migration` proves this fixes it.
        //
        // What this does **not** promise: OpenUpgrade ships real data
        // migration scripts only for Odoo's own core and OCA-tracked
        // modules. A private module gets the generic `-u` treatment any
        // addon gets — `_auto_init` re-run, views reloaded — with no
        // dedicated migration unless someone wrote one. Carrying the code
        // turns a guaranteed failure into a real attempt; it does not
        // guarantee the attempt succeeds. `UpgradePlan::carried_addons`
        // says exactly which sources are riding along, so this is a
        // visible bet, not a silent assumption.
        let run_dir = app_dir.join("upgrades").join(working.id.to_string()).join(&hop.to);
        std::fs::create_dir_all(&run_dir)?;
        let admin_conn = self.pg_conn_info_for_running_instance(postgres_instance_id)?;
        pg_admin::ensure_role(&admin_conn, "odoo").await?;
        pg_admin::grant_schema_privileges(&admin_conn, &working.name, "odoo").await?;
        let odoo_conn = pg_admin::PgConnInfo { user: "odoo".to_string(), ..admin_conn };

        let mut addons_path = vec![
            runtime.checkout_root.join("addons").display().to_string(),
            runtime.checkout_root.join("odoo").join("addons").display().to_string(),
            openupgrade_root.display().to_string(),
        ];
        addons_path.extend(self.carried_addons_for(working.server_id)?.into_iter().map(|c| c.path_or_url));

        let conf = odoo_conf::render(&odoo_conf::OdooConfParams {
            addons_path,
            db_host: &odoo_conn.host,
            db_port: odoo_conn.port,
            db_user: &odoo_conn.user,
            http_port: 0,
            data_dir: &run_dir.join("filestore"),
        });
        let conf_path = run_dir.join("odoo.conf");
        std::fs::write(&conf_path, conf)?;

        let odoo_bin = runtime.checkout_root.join("odoo-bin");
        let args = vec![
            odoo_bin.display().to_string(),
            "-c".to_string(),
            conf_path.display().to_string(),
            "-d".to_string(),
            working.name.clone(),
            "--update".to_string(),
            "all".to_string(),
            // `openupgrade_framework` patches Odoo's module loading before
            // the registry is built, so it has to be server-wide rather
            // than merely installed. This exact incantation is OpenUpgrade's
            // own documented command line.
            "--load=base,web,openupgrade_framework".to_string(),
            "--stop-after-init".to_string(),
            "--no-http".to_string(),
        ];
        // Lets OpenUpgrade skip work that only matters for a later target.
        let env = vec![("OPENUPGRADE_TARGET_VERSION".to_string(), hop.to.clone())];

        let output = odoo::run_one_shot(&venv_python, &runtime.checkout_root, &args, &env, None).await?;
        if !output.success {
            return Err(CoreError::ModuleCommandFailed {
                database: working.name.clone(),
                action: format!("migrate {} → {}", hop.from, hop.to),
                stdout: output.stdout,
                stderr: output.stderr,
            });
        }

        // Trust the database, not the exit code: OpenUpgrade can exit 0
        // having logged a failure, and `base`'s own recorded version is the
        // thing that says which Odoo this database now belongs to.
        let installed = pg_admin::query_scalar(
            &odoo_conn,
            &working.name,
            "SELECT latest_version FROM ir_module_module WHERE name = 'base'",
        )
        .await?
        .unwrap_or_default();
        if !installed.starts_with(&hop.to) {
            return Err(CoreError::NotConfigured(format!(
                "the migration to {} reported success, but base is still at {installed}",
                hop.to
            )));
        }
        Ok(())
    }

    // --- is my code actually live? ----------------------------------------
    //
    // The old answer compared the version in `__manifest__.py` against the
    // version Odoo has installed. That only notices a change when somebody
    // remembered to bump the manifest, and nobody bumps it while
    // iterating — so the screen had to say, honestly, that it couldn't
    // really tell. Hashing the module's files answers it properly.

    /// Notes what each module's files hash to right now, as the state this
    /// database was last given.
    fn remember_module_contents(&self, database_id: Uuid, module_names: &[String]) -> Result<(), CoreError> {
        let database = self.get_database(database_id)?;
        let scan = self.scan_modules(database.server_id)?;
        for name in module_names {
            if let Some(found) = scan.modules.iter().find(|m| &m.technical_name == name) {
                if let Some(hash) = modules::content_hash(&found.path) {
                    self.db.record_module_install(database_id, name, &hash)?;
                }
            }
        }
        Ok(())
    }

    /// The addons folders under `path`, with a proposed load order and a
    /// stated reason for each guess. Nothing is registered — this is what
    /// the app would suggest, for a person to accept or argue with.
    pub fn discover_addons_roots(&self, path: &std::path::Path) -> Vec<modules::DiscoveredRoot> {
        // Four levels is enough for the shapes that actually occur — a
        // workspace of repositories, and an Odoo checkout's own
        // `odoo/addons` — without walking somebody's entire home folder.
        modules::discover_addons_roots(path, 4)
    }

    /// Registers what `discover_addons_roots` found, in the order it
    /// proposed. Existing sources are left alone and their paths are never
    /// added twice.
    pub fn adopt_addons_roots(
        &self,
        server_id: Uuid,
        roots: &[modules::DiscoveredRoot],
    ) -> Result<Vec<AddonsSource>, CoreError> {
        let existing = self.db.list_addons_sources(server_id)?;
        let taken: std::collections::HashSet<String> = existing.iter().map(|s| s.path_or_url.clone()).collect();
        let mut rank = existing.iter().map(|s| s.rank + 1).max().unwrap_or(0);
        let mut added = Vec::new();
        for root in roots {
            let path = root.path.to_string_lossy().to_string();
            if taken.contains(&path) {
                continue;
            }
            added.push(self.create_addons_source(server_id, root.label.clone(), path, root.kind, rank)?);
            rank += 1;
        }
        Ok(added)
    }

    /// Which installed modules have files on disk that differ from the ones
    /// this database was last given.
    ///
    /// Deliberately says nothing about a module installed before this app
    /// started recording hashes: `Unknown` is its own answer, and rendering
    /// it as "up to date" would be the lie that made version-comparison
    /// unusable in the first place.
    pub async fn module_freshness(&self, database_id: Uuid) -> Result<Vec<ModuleFreshness>, CoreError> {
        let database = self.get_database(database_id)?;
        let scan = self.scan_modules(database.server_id)?;
        let recorded = self.db.module_installs(database_id)?;
        let states = self.database_module_states(database_id).await?;

        let mut out = Vec::new();
        for state in states.into_iter().filter(|s| s.state == "installed") {
            let on_disk = scan
                .modules
                .iter()
                .find(|m| m.technical_name == state.technical_name)
                .and_then(|m| modules::content_hash(&m.path));
            let freshness = match (recorded.get(&state.technical_name), on_disk) {
                (Some(was), Some(now)) if *was == now => Freshness::Live,
                (Some(_), Some(_)) => Freshness::Changed,
                // Installed by something other than this app, or before it
                // started recording. We genuinely don't know.
                (None, Some(_)) => Freshness::Unrecorded,
                // Installed in the database but its folder isn't on the
                // addons path any more — a real situation, and not one to
                // paper over.
                (_, None) => Freshness::NotOnDisk,
            };
            out.push(ModuleFreshness { technical_name: state.technical_name, freshness });
        }
        out.sort_by(|a, b| a.technical_name.cmp(&b.technical_name));
        Ok(out)
    }

    // --- the template cache -----------------------------------------------
    //
    // Creating a database used to hand back a bare Postgres database that
    // wasn't an Odoo database at all: you then went to Apps, installed
    // something, and waited out a full `-i` boot. Every time.
    //
    // `CREATE DATABASE ... TEMPLATE` copies an initialized database in
    // seconds, and this app already uses that mechanism for snapshots and
    // duplicates. So the first database of a given shape pays the boot, and
    // every one after it is a copy. This is `click-odoo-initdb`'s trick,
    // which the research doc named as one of the two most reusable
    // primitives in the Odoo tooling world.

    /// What a cached template is *of*: an Odoo version, a set of modules,
    /// and the contents of those modules. Change any of the three and the
    /// old template is the wrong answer, so the key changes with it.
    fn template_key(&self, server_id: Uuid, modules: &[String]) -> Result<String, CoreError> {
        use sha2::{Digest, Sha256};
        let server = self.get_server(server_id)?;
        let mut sorted: Vec<&String> = modules.iter().collect();
        sorted.sort();

        let scan = self.scan_modules(server_id)?;
        let mut hasher = Sha256::new();
        hasher.update(server.odoo_version.as_bytes());
        for name in &sorted {
            hasher.update(name.as_bytes());
            hasher.update([0]);
            // The module's own contents, so editing a module invalidates
            // every template built from it. Without this the cache would
            // confidently hand back yesterday's schema.
            if let Some(found) = scan.modules.iter().find(|m| &&m.technical_name == name) {
                if let Some(hash) = modules::content_hash(&found.path) {
                    hasher.update(hash.as_bytes());
                }
            }
            hasher.update([0]);
        }
        // Short, because it becomes part of a Postgres identifier and those
        // are capped at 63 bytes.
        Ok(format!("{:x}", hasher.finalize())[..16].to_string())
    }

    fn template_database_name(key: &str) -> String {
        format!("oo_template_{key}")
    }

    /// Creates a database that is a usable Odoo database immediately.
    ///
    /// The first call for a given (version, modules, module contents) pays
    /// a real `-i` boot and keeps the result as a template; every later call
    /// is a `CREATE DATABASE ... TEMPLATE`, which is seconds.
    ///
    /// Returns the new database and whether the cache was used, because a
    /// UI that says "instant" when it actually spent a minute booting Odoo
    /// is the kind of small lie this project keeps removing.
    pub async fn create_database_with_modules(
        &self,
        server_id: Uuid,
        name: impl Into<String>,
        modules: &[String],
    ) -> Result<(Database, bool), CoreError> {
        let name = name.into();
        if modules.is_empty() {
            return Ok((self.create_database(server_id, name).await?, false));
        }

        let key = self.template_key(server_id, modules)?;
        let template = Self::template_database_name(&key);
        let server = self.get_server(server_id)?;
        let conn = self.pg_conn_info_for_running_instance(server.postgres_instance_id)?;

        if pg_admin::database_exists(&conn, &template).await? {
            let started = std::time::Instant::now();
            pg_admin::duplicate_database(&conn, &template, &name).await?;
            let database = Database::new(server_id, name);
            self.db.insert_database(&database)?;
            self.record_timed_event(
                Event::DatabaseCreated { database_id: database.id, server_id, name: database.name.clone() },
                started.elapsed(),
            )?;
            return Ok((database, true));
        }

        // Cold: build it for real, once.
        let database = self.create_database(server_id, name).await?;
        self.install_modules_here(database.id, modules).await?;

        // Then keep a copy for next time. Deliberately not fatal: failing
        // to populate a cache must never fail the thing the user asked for.
        if let Err(err) = pg_admin::duplicate_database(&conn, &database.name, &template).await {
            tracing::warn!("created {} but couldn't cache it as a template: {err}", database.name);
        } else {
            self.record_event(Event::TemplateCached { key: key.clone(), modules: modules.to_vec() })?;
        }
        Ok((database, false))
    }

    /// Every cached template, with what it cost to keep.
    pub async fn cached_templates(&self) -> Result<Vec<CachedTemplate>, CoreError> {
        let mut out = Vec::new();
        for instance in self.db.list_postgres_instances()? {
            let Ok(found) = self.list_cluster_databases(instance.id).await else { continue };
            for database in found {
                if let Some(key) = database.name.strip_prefix("oo_template_") {
                    out.push(CachedTemplate {
                        key: key.to_string(),
                        database_name: database.name.clone(),
                        instance_id: instance.id,
                        size_bytes: database.size_bytes,
                    });
                }
            }
        }
        Ok(out)
    }

    /// Throws away every cached template. They rebuild themselves on next
    /// use, so this only ever costs time — which is why it needs no
    /// confirmation beyond the one the Settings screen already asks for.
    pub async fn clear_template_cache(&self) -> Result<u64, CoreError> {
        let mut freed = 0;
        for template in self.cached_templates().await? {
            let Ok(conn) = self.pg_conn_info_for_running_instance(template.instance_id) else { continue };
            if pg_admin::drop_database(&conn, &template.database_name).await.is_ok() {
                freed += template.size_bytes;
            }
        }
        Ok(freed)
    }

    // --- neutralization -------------------------------------------------
    //
    // Restoring a client's production dump onto a laptop is the single most
    // dangerous thing this app makes easy. The dump carries their live
    // outgoing mail servers *with working passwords*, every scheduled job,
    // and real payment-provider credentials — so a local copy left running
    // can email their customers, charge real cards, and pull their inbox.
    //
    // Odoo solved this and this app was ignoring it: `odoo-bin neutralize`
    // runs a `data/neutralize.sql` shipped by each installed module (56 of
    // them in Odoo 17 core alone). Delegated rather than reimplemented, for
    // the same reason `--dev=reload` is: Odoo's list changes per version and
    // per installed module, and a hand-copied list of tables would be
    // wrong the first time a module was added.

    /// Runs Odoo's own neutralization over one database.
    ///
    /// Verified against a real Odoo 17: an SMTP server with a working
    /// password ends up deactivated *and* its credentials nulled, a dummy
    /// unreachable mail server is inserted so no command-line fallback can
    /// take over, active crons drop from 12 to 1 (autovacuum), and Odoo
    /// sets `ir_config_parameter['database.is_neutralized'] = true`.
    /// Same posture as `install_modules_here`: the app knows its own layout,
    /// so nobody has to tell it where Odoo lives.
    pub async fn neutralize_database_here(&self, database_id: Uuid) -> Result<(), CoreError> {
        let database = self.get_database(database_id)?;
        let (root, python, run_dir) = self.resolve_runtime_for(database.server_id).await?;
        self.neutralize_database(database_id, root, python, run_dir).await
    }

    pub async fn neutralize_database(
        &self,
        database_id: Uuid,
        checkout_root: impl Into<std::path::PathBuf>,
        venv_python: impl Into<std::path::PathBuf>,
        runtime_dir: impl Into<std::path::PathBuf>,
    ) -> Result<(), CoreError> {
        let started = std::time::Instant::now();
        let venv_python = venv_python.into();
        let (database, checkout_root, conf_path, admin_conn) =
            self.prepare_odoo_runtime_for_database(database_id, checkout_root, runtime_dir).await?;

        // A database no Odoo has ever initialized has nothing to
        // neutralize. That is a *success*, not a failure: restoring a dump
        // of a bare database and asking for it to be made safe should not
        // hand back an error that teaches people to untick the box.
        if pg_admin::query_scalar(
            &admin_conn,
            &database.name,
            "SELECT to_regclass('public.ir_module_module') IS NOT NULL",
        )
        .await
        .ok()
        .flatten()
        .as_deref()
            != Some("t")
        {
            tracing::info!("{} has no Odoo tables, so there is nothing to neutralize", database.name);
            return Ok(());
        }

        let odoo_bin = checkout_root.join("odoo-bin");
        let args = vec![
            odoo_bin.display().to_string(),
            // A subcommand, not a flag: `odoo-bin neutralize` is one of
            // Odoo's `cli` commands, so the word comes before the options.
            "neutralize".to_string(),
            "-c".to_string(),
            conf_path.display().to_string(),
            "-d".to_string(),
            database.name.clone(),
        ];
        let output = odoo::run_one_shot(&venv_python, &checkout_root, &args, &[], None).await?;
        if !output.success {
            return Err(CoreError::ModuleCommandFailed {
                database: database.name,
                action: "neutralize".to_string(),
                stdout: output.stdout,
                stderr: output.stderr,
            });
        }
        self.record_timed_event(Event::DatabaseNeutralized { database_id }, started.elapsed())?;
        Ok(())
    }

    /// Whether each database is neutralized, **read from the databases
    /// themselves** rather than from anything this app remembers.
    ///
    /// Odoo writes `database.is_neutralized` into `ir_config_parameter`, so
    /// that flag is true for a database neutralized outside this app, and
    /// for one restored from a dump that was already neutralized before it
    /// ever arrived. A column here would be this app's opinion; this is the
    /// fact.
    pub async fn neutralization_states(&self) -> Result<Vec<(Uuid, NeutralizationState)>, CoreError> {
        let mut out = Vec::new();
        for database in self.list_databases(None)? {
            let server = match self.get_server(database.server_id) {
                Ok(server) => server,
                Err(_) => continue,
            };
            // A stopped cluster isn't an answer of "no" — it's no answer,
            // and saying "not neutralized" because we couldn't look would
            // be the worst possible lie for this particular question.
            let Ok(conn) = self.pg_conn_info_for_running_instance(server.postgres_instance_id) else {
                out.push((database.id, NeutralizationState::Unknown));
                continue;
            };
            let state = match pg_admin::query_scalar(
                &conn,
                &database.name,
                "SELECT value FROM ir_config_parameter WHERE key = 'database.is_neutralized'",
            )
            .await
            {
                Ok(Some(value)) if value.eq_ignore_ascii_case("true") => NeutralizationState::Neutralized,
                Ok(_) => NeutralizationState::Live,
                // The table doesn't exist, so no Odoo has ever initialized
                // this database. Nothing to neutralize and nothing to warn
                // about.
                Err(_) => NeutralizationState::NotOdoo,
            };
            out.push((database.id, state));
        }
        Ok(out)
    }

    /// Installs `module_names` into `database_id` — a bare (never-Odoo-
    /// initialized) database is a completely valid input: real `odoo-bin -i`
    /// bootstraps the whole framework (`base` and every dependency) as part
    /// of installing the first module, exactly like a real production
    /// database's very first install would.
    pub async fn install_modules(
        &self,
        database_id: Uuid,
        module_names: &[String],
        checkout_root: impl Into<std::path::PathBuf>,
        venv_python: impl Into<std::path::PathBuf>,
        runtime_dir: impl Into<std::path::PathBuf>,
    ) -> Result<(), CoreError> {
        let (database, _admin_conn, output) =
            self.run_module_flag_command(database_id, module_names, "-i", checkout_root, venv_python, runtime_dir).await?;
        if !output.success {
            return Err(CoreError::ModuleCommandFailed {
                database: database.name,
                action: "install".to_string(),
                stdout: output.stdout,
                stderr: output.stderr,
            });
        }
        for name in module_names {
            self.record_event(Event::ModuleInstalled { database_id, technical_name: name.clone() })?;
        }
        self.remember_module_contents(database_id, module_names)?;
        Ok(())
    }

    /// Upgrades already-installed `module_names` on `database_id`. Reports
    /// each module's real, post-upgrade `ir_module_module.latest_version`
    /// on the emitted event — not a fabricated one, per this project's
    /// standing "no fake progress" rule — by actually reading it back via
    /// `pg_admin::query_scalar` after the upgrade succeeds.
    pub async fn upgrade_modules(
        &self,
        database_id: Uuid,
        module_names: &[String],
        checkout_root: impl Into<std::path::PathBuf>,
        venv_python: impl Into<std::path::PathBuf>,
        runtime_dir: impl Into<std::path::PathBuf>,
    ) -> Result<(), CoreError> {
        let (database, admin_conn, output) =
            self.run_module_flag_command(database_id, module_names, "-u", checkout_root, venv_python, runtime_dir).await?;
        if !output.success {
            return Err(CoreError::ModuleCommandFailed {
                database: database.name,
                action: "upgrade".to_string(),
                stdout: output.stdout,
                stderr: output.stderr,
            });
        }
        for name in module_names {
            let sql = format!("SELECT latest_version FROM ir_module_module WHERE name = '{}'", name.replace('\'', "''"));
            let to_version = pg_admin::query_scalar(&admin_conn, &database.name, &sql).await?.unwrap_or_else(|| "unknown".to_string());
            self.record_event(Event::ModuleUpgraded { database_id, technical_name: name.clone(), to_version })?;
        }
        self.remember_module_contents(database_id, module_names)?;
        Ok(())
    }

    /// Uninstalls `module_names` from `database_id`. Real `odoo-bin` has no
    /// CLI flag for this (only `-i`/`-u`), so this is the one module command
    /// that goes through `odoo-bin shell` instead, feeding it a real
    /// `ir.module.module.button_immediate_uninstall()` ORM call on stdin —
    /// the standard non-interactive way to do this, same mechanism
    /// `click-odoo-contrib`'s own tooling uses under the hood.
    pub async fn uninstall_modules(
        &self,
        database_id: Uuid,
        module_names: &[String],
        checkout_root: impl Into<std::path::PathBuf>,
        venv_python: impl Into<std::path::PathBuf>,
        runtime_dir: impl Into<std::path::PathBuf>,
    ) -> Result<(), CoreError> {
        let (database, checkout_root, conf_path, _admin_conn) =
            self.prepare_odoo_runtime_for_database(database_id, checkout_root, runtime_dir).await?;
        let odoo_bin = checkout_root.join("odoo-bin");
        let names_literal: Vec<String> = module_names.iter().map(|n| format!("'{}'", n.replace('\'', "\\'"))).collect();
        let script = format!(
            "records = env['ir.module.module'].search([('name', 'in', [{names}])])\n\
             records.button_immediate_uninstall()\n\
             env.cr.commit()\n",
            names = names_literal.join(", "),
        );
        let args = vec![
            odoo_bin.display().to_string(),
            "shell".to_string(),
            "-c".to_string(),
            conf_path.display().to_string(),
            "-d".to_string(),
            database.name.clone(),
            "--no-http".to_string(),
        ];
        let output = odoo::run_one_shot(&venv_python.into(), &checkout_root, &args, &[], Some(&script)).await?;
        if !output.success {
            return Err(CoreError::ModuleCommandFailed {
                database: database.name,
                action: "uninstall".to_string(),
                stdout: output.stdout,
                stderr: output.stderr,
            });
        }
        for name in module_names {
            self.record_event(Event::ModuleUninstalled { database_id, technical_name: name.clone() })?;
        }
        Ok(())
    }

    /// Reads `database_id`'s real `ir_module_module` state — every module
    /// Odoo itself knows about on this database, not just installed ones —
    /// straight from Postgres. Returns an empty list, not an error, for a
    /// database that was never Odoo-initialized (no `ir_module_module`
    /// table at all yet): that's a normal, honest "nothing installed here
    /// yet" case (see `install_modules`'s own doc comment on why a bare
    /// database is a valid input), not a failure — `query_rows` reports
    /// exactly one specific Postgres error for it (`relation
    /// "ir_module_module" does not exist`), detected here rather than
    /// masking every other real failure the same way.
    pub async fn database_module_states(&self, database_id: Uuid) -> Result<Vec<DatabaseModuleState>, CoreError> {
        let database = self.get_database(database_id)?;
        let server = self.get_server(database.server_id)?;
        let conn = self.pg_conn_info_for_running_instance(server.postgres_instance_id)?;

        // `latest_version` is the real stored column (see `DatabaseModuleState`'s
        // doc comment for why it maps to our `installed_version` field, not
        // Odoo's own compute-only `installed_version` attribute, which isn't
        // a column at all).
        let rows = match pg_admin::query_rows(&conn, &database.name, "SELECT name, state, latest_version FROM ir_module_module ORDER BY name").await {
            Ok(rows) => rows,
            Err(pg_admin::PgAdminError::CommandFailed { stderr, .. }) if stderr.contains("ir_module_module") && stderr.contains("does not exist") => {
                return Ok(Vec::new());
            }
            Err(err) => return Err(err.into()),
        };

        Ok(rows
            .into_iter()
            .filter_map(|row| {
                let [name, state, installed_version] = <[String; 3]>::try_from(row).ok()?;
                Some(DatabaseModuleState { technical_name: name, state, installed_version: if installed_version.is_empty() { None } else { Some(installed_version) } })
            })
            .collect())
    }

    // --- postgres instances ------------------------------------------------
    //
    // Deliberately plural and unlimited — see `PostgresInstance`'s doc comment.
    // An app can register as many of these as it wants (different major
    // versions under test, or a user who just wants isolation); nothing here
    // assumes exactly one.

    /// Register a new Postgres instance's static config and emit
    /// `PostgresInstanceCreated`. Doesn't start it — that's a separate call,
    /// same split as `create_server` vs. actually starting an Odoo process.
    pub fn create_postgres_instance(
        &self,
        label: impl Into<String>,
        pg_version: impl Into<String>,
        port: u16,
        data_dir: impl Into<String>,
    ) -> Result<PostgresInstance, CoreError> {
        let instance = PostgresInstance::new(label, pg_version, port, data_dir);
        self.db.insert_postgres_instance(&instance)?;
        self.record_event(Event::PostgresInstanceCreated {
            instance_id: instance.id,
            label: instance.label.clone(),
            pg_version: instance.pg_version.clone(),
        })?;
        Ok(instance)
    }

    pub fn list_postgres_instances(&self) -> Result<Vec<PostgresInstance>, CoreError> {
        self.db.list_postgres_instances()
    }

    pub fn get_postgres_instance(&self, id: Uuid) -> Result<PostgresInstance, CoreError> {
        self.db.get_postgres_instance(id)?.ok_or_else(|| CoreError::NotFound(format!("postgres instance {id}")))
    }

    /// Get the live `PgSupervisor` for `id`, constructing and registering one
    /// on first access if it isn't already in the in-memory map. Concurrent
    /// first-access from two callers is resolved by `HashMap::entry` — both
    /// build a `PgSupervisor`, but only the one that wins `or_insert` is ever
    /// used or started, so there's never a second live supervisor for the
    /// same instance id.
    fn get_or_create_supervisor(&self, id: Uuid) -> Result<postgres::PgSupervisor, CoreError> {
        {
            let map = self.postgres_supervisors.lock().expect("postgres supervisors mutex poisoned");
            if let Some(sup) = map.get(&id) {
                return Ok(sup.clone());
            }
        }

        // Not registered yet — load its static config from disk and build a
        // fresh supervisor before taking the lock again, so we're not holding
        // it across a filesystem lookup.
        let instance = self.get_postgres_instance(id)?;
        let bin_dir = postgres::discover_system_bin_dir().ok_or_else(|| {
            CoreError::NotFound(
                "no Postgres binaries found (dev/test environments need a system Postgres install; \
                 the shipped app bundles its own runtime instead)"
                    .to_string(),
            )
        })?;
        let mut config = postgres::PgConfig::new(bin_dir, instance.data_dir.clone(), instance.port);
        // Dev/sandbox-only: see `sandbox_root_workaround_user`'s doc comment.
        // A no-op on every real desktop install (which never runs as root);
        // without it, this project's own root-only CI/sandbox couldn't start
        // real Postgres processes at the `Core` level at all.
        #[cfg(unix)]
        if let Some((uid, gid)) = postgres::sandbox_root_workaround_user() {
            if std::fs::create_dir_all(&config.data_dir).is_ok()
                && std::os::unix::fs::chown(&config.data_dir, Some(uid), Some(gid)).is_ok()
            {
                config.run_as = Some((uid, gid));
            }
        }

        let core = self.clone();
        let sup = postgres::PgSupervisor::with_listener(config, move |state| {
            // Best-effort: if recording the event fails (e.g. db closed during
            // shutdown), there's no one left to report the error to — this
            // closure can't return a Result to any caller.
            let event = match state {
                PgState::Stopped => Event::PostgresStopped { instance_id: id },
                PgState::Starting => Event::PostgresStarting { instance_id: id },
                PgState::Running { pid } => Event::PostgresRunning { instance_id: id, pid },
                PgState::Stopping => Event::PostgresStopping { instance_id: id },
                PgState::Crashed { exit_code } => Event::PostgresCrashed { instance_id: id, exit_code },
            };
            let _ = core.record_event(event);
        });

        let mut map = self.postgres_supervisors.lock().expect("postgres supervisors mutex poisoned");
        Ok(map.entry(id).or_insert(sup).clone())
    }

    pub async fn start_postgres_instance(&self, id: Uuid) -> Result<(), CoreError> {
        let sup = self.get_or_create_supervisor(id)?;
        sup.start().await?;
        Ok(())
    }

    pub async fn stop_postgres_instance(&self, id: Uuid) -> Result<(), CoreError> {
        let sup = self.get_or_create_supervisor(id)?;
        sup.stop().await?;
        Ok(())
    }

    /// Live state, not the DB row — `Stopped` for an instance that's never
    /// been started this process run (there's no supervisor for it yet), and
    /// whatever the real supervisor reports otherwise. Validates the instance
    /// actually exists first so an unknown id gives `NotFound`, not a
    /// misleading `Stopped`.
    pub fn postgres_instance_state(&self, id: Uuid) -> Result<PgState, CoreError> {
        self.get_postgres_instance(id)?;
        let map = self.postgres_supervisors.lock().expect("postgres supervisors mutex poisoned");
        Ok(map.get(&id).map(|sup| sup.state()).unwrap_or(PgState::Stopped))
    }
}

#[cfg(test)]
mod tests {
    use super::*;


    /// A backup is only a safety net if you can find it again.
    #[tokio::test]
    async fn a_backup_can_be_found_again_by_the_app_that_took_it() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        let database = core.create_database(server.id, "acme_live").await.unwrap();

        let backups = tempfile::TempDir::new().unwrap();
        assert!(core.list_backups(backups.path()).unwrap().is_empty(), "nothing taken yet");

        core.backup_database(database.id, backups.path()).await.unwrap();
        let found = core.list_backups(backups.path()).unwrap();
        assert_eq!(found.len(), 1);
        // The name has to survive the round trip through the filename, or
        // the list is a row of paths nobody can read.
        assert_eq!(found[0].database_name, "acme_live");
        assert_eq!(found[0].database_id, Some(database.id));
        assert!(found[0].source_still_exists);
        assert!(found[0].size_bytes > 0);

        // The case the list exists for: the database is gone, the backup
        // is not.
        core.drop_database(database.id).await.unwrap();
        let after = core.list_backups(backups.path()).unwrap();
        assert_eq!(after.len(), 1, "dropping the database must not lose its backup");
        assert!(!after[0].source_still_exists, "and the list must say the original is gone");
    }

    /// Listing reads the folder, so a file removed behind the app's back
    /// stops being offered rather than being confidently named.
    #[test]
    fn a_backup_deleted_outside_the_app_stops_being_listed() {
        let core = Core::open(":memory:").unwrap();
        let backups = tempfile::TempDir::new().unwrap();
        let path = backups.path().join(format!("acme-{}.sql", Uuid::new_v4()));
        std::fs::write(&path, "-- dump").unwrap();
        assert_eq!(core.list_backups(backups.path()).unwrap().len(), 1);
        std::fs::remove_file(&path).unwrap();
        assert!(core.list_backups(backups.path()).unwrap().is_empty());
    }

    /// The filename is `<name>-<uuid>.sql`, and a uuid is full of dashes —
    /// so splitting on the last dash finds the uuid's last group, not the
    /// name boundary. This is the test that caught that.
    #[test]
    fn a_database_name_with_dashes_still_reads_back_correctly() {
        let core = Core::open(":memory:").unwrap();
        let backups = tempfile::TempDir::new().unwrap();
        let id = Uuid::new_v4();
        std::fs::write(backups.path().join(format!("acme-retail-2024-{id}.sql")), "-- dump").unwrap();
        let found = core.list_backups(backups.path()).unwrap();
        assert_eq!(found[0].database_name, "acme-retail-2024");
        assert_eq!(found[0].database_id, Some(id));
    }

    /// And a file that isn't one of ours is described from its own name
    /// rather than skipped — someone who dropped a dump in the folder by
    /// hand should still see it offered.
    #[test]
    fn a_dump_this_app_did_not_write_is_still_listed() {
        let core = Core::open(":memory:").unwrap();
        let backups = tempfile::TempDir::new().unwrap();
        std::fs::write(backups.path().join("from_the_client.sql"), "-- dump").unwrap();
        std::fs::write(backups.path().join("notes.txt"), "not a dump").unwrap();
        let found = core.list_backups(backups.path()).unwrap();
        assert_eq!(found.len(), 1, "only .sql files are dumps");
        assert_eq!(found[0].database_name, "from_the_client");
        assert_eq!(found[0].database_id, None);
        assert!(!found[0].source_still_exists);
    }

    /// The one event whose subject no longer exists by the time anyone
    /// reads it. If the name isn't on the event, the log says "dropped
    /// database 3f2a1b0c" — accurate, useless, and exactly the row someone
    /// goes looking for after a bad afternoon.
    #[tokio::test]
    async fn a_dropped_database_is_still_named_in_the_log_afterwards() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        let database = core.create_database(server.id, "acme_live").await.unwrap();
        core.drop_database(database.id).await.unwrap();

        let events = core.recent_events(50).unwrap();
        let dropped = events
            .iter()
            .find_map(|e| match &e.event {
                Event::DatabaseDropped { database_id, server_id, name } if *database_id == database.id => {
                    Some((*server_id, name.clone()))
                }
                _ => None,
            })
            .expect("the drop should be in the log");
        assert_eq!(dropped.1, "acme_live", "the name has to be on the event; nothing else remembers it");
        // And the server, because restoring it from a backup needs to know
        // where to put it back.
        assert_eq!(dropped.0, server.id);

        // The row really is gone — this isn't passing because the lookup
        // still works.
        assert!(core.get_database(database.id).is_err());
    }

    /// Same reason, one step earlier: a backup row can only offer a restore
    /// if it says where the dump went *and* which Odoo it belongs to.
    #[tokio::test]
    async fn a_backup_event_says_where_the_dump_went_and_where_it_belongs() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        let database = core.create_database(server.id, "acme_live").await.unwrap();
        let backups = tempfile::TempDir::new().unwrap();
        let written = core.backup_database(database.id, backups.path()).await.unwrap();

        let events = core.recent_events(50).unwrap();
        let (server_id, path) = events
            .iter()
            .find_map(|e| match &e.event {
                Event::DatabaseBackedUp { server_id, path, .. } => Some((*server_id, path.clone())),
                _ => None,
            })
            .expect("the backup should be in the log");
        assert_eq!(server_id, server.id);
        assert_eq!(std::path::Path::new(&path), written, "the recorded path must be the file that exists");
        assert!(written.is_file());
    }

    // --- major-version migration ------------------------------------------

    /// A real OpenUpgrade migration: a real 16.0 database, migrated to 17.0
    /// by real `odoo-bin --update all`, verified by asking the database
    /// which version it now thinks it is.
    ///
    /// Slow and heavy — two Odoo checkouts, two venvs and a real migration
    /// run — so it's opt-in via `ORCHESTRATOR_TEST_REAL_UPGRADE=1`. It is
    /// the only thing that can tell us this feature works at all, which is
    /// why it exists rather than a mock that proves the command line was
    /// spelled correctly.
    #[tokio::test]
    async fn a_real_database_really_migrates_a_major_version() {
        if std::env::var("ORCHESTRATOR_TEST_REAL_UPGRADE").is_err() {
            eprintln!("skipping: set ORCHESTRATOR_TEST_REAL_UPGRADE=1 (needs two Odoo checkouts and several minutes)");
            return;
        }
        let Some((core, _server, dir)) = core_with_running_postgres_and_server().await else {
            panic!("this test was asked for explicitly, so a missing Postgres is a failure, not a skip");
        };
        eprintln!("[upgrade] postgres up");
        core.with_environment(dir.path(), vec![]);

        // A 16.0 Odoo, because that's what we're migrating *from*.
        let old = core.create_server("legacy", "16.0", 8069, _server.postgres_instance_id).unwrap();
        eprintln!("[upgrade] fetching Odoo 16…");
        let runtime = core.ensure_odoo("16.0").await.expect("Odoo 16 should be fetchable");
        eprintln!("[upgrade] Odoo 16 at {}", runtime.checkout_root.display());
        core.create_addons_source(old.id, "core", runtime.checkout_root.join("addons").to_string_lossy(), SourceKind::Core, 0)
            .unwrap();
        core.create_addons_source(
            old.id,
            "base",
            runtime.checkout_root.join("odoo").join("addons").to_string_lossy(),
            SourceKind::Core,
            1,
        )
        .unwrap();

        let database = core.create_database(old.id, "legacy_db").await.unwrap();
        eprintln!("[upgrade] installing base on 16.0…");
        core.install_modules_here(database.id, &["base".to_string()]).await.expect("a real 16.0 database");
        eprintln!("[upgrade] 16.0 database ready");

        let conn = core.pg_conn_info_for_running_instance(old.postgres_instance_id).unwrap();
        let before = pg_admin::query_scalar(&conn, "legacy_db", "SELECT latest_version FROM ir_module_module WHERE name='base'")
            .await
            .unwrap()
            .unwrap();
        assert!(before.starts_with("16.0"), "the starting point has to really be 16.0, not {before}");

        // Put something recognisable in it, so "the data survived" is a
        // fact rather than an assumption.
        pg_admin::execute(&conn, "legacy_db", "INSERT INTO res_partner (name, active, company_id) VALUES ('Acme Client', true, 1)")
            .await
            .unwrap();

        let plan = core.plan_upgrade(database.id, "17.0").await.unwrap();
        assert_eq!(plan.hops.len(), 1, "16 → 17 is one hop");
        assert_eq!(plan.into, "legacy_db_v170");

        eprintln!("[upgrade] running the migration…");
        let run = core.run_upgrade(database.id, "17.0").await.unwrap();
        eprintln!("[upgrade] run finished: completed {}, failed {:?}", run.completed.len(), run.failed.as_ref().map(|f| &f.message));
        assert!(run.failed.is_none(), "migration failed: {:?}", run.failed);
        assert_eq!(run.completed.len(), 1);

        let working = core.get_database(run.working_database_id.unwrap()).unwrap();
        let after = pg_admin::query_scalar(&conn, &working.name, "SELECT latest_version FROM ir_module_module WHERE name='base'")
            .await
            .unwrap()
            .unwrap();
        assert!(after.starts_with("17.0"), "the database should now be 17.0, not {after}");

        // The data came with it.
        let partner = pg_admin::query_scalar(&conn, &working.name, "SELECT count(*) FROM res_partner WHERE name = 'Acme Client'")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(partner, "1", "the migration must bring the data, not just the schema");

        // And the original is exactly as it was — the entire reason to use
        // this rather than running OpenUpgrade by hand.
        let original = pg_admin::query_scalar(&conn, "legacy_db", "SELECT latest_version FROM ir_module_module WHERE name='base'")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(original, before, "the original database must not have been touched");

        // Every hop left a checkpoint to come back to.
        let snapshots = core.list_snapshots(Some(working.id)).unwrap();
        assert!(snapshots.iter().any(|s| s.name.contains("16.0 → 17.0")), "{:?}", snapshots.iter().map(|s| &s.name).collect::<Vec<_>>());
    }

    /// The top open question this project's own docs flagged: a real client
    /// database almost always has a custom module installed, and the
    /// migration used to leave the server's own addons sources off the
    /// addons path entirely. That isn't "the custom module doesn't get
    /// migrated" — it's `--update all` unable to *find* an installed
    /// module's code at all, which fails the whole hop.
    ///
    /// This proves the fix: a private module, installed before the
    /// migration, rides along on the addons path and survives a real
    /// 16.0 → 17.0 run, data and all — while `plan.carried_addons` says so
    /// up front rather than the app finding out mid-run.
    ///
    /// A second real finding fell out of getting this to pass: Odoo 17
    /// refuses to even *parse* a module manifest whose declared version
    /// doesn't start with the running series — `16.0.1.0.0`, the version
    /// almost every real custom or OCA module actually declares, is a hard
    /// "invalid manifest" on a 17.0 Odoo, series-carried-along or not. This
    /// module deliberately declares a bare `1.0.0` instead, which is the
    /// one form Odoo accepts unmodified under any series. A real client's
    /// modules will need their manifest versions bumped to the target
    /// series before a hop that carries them can even load them — a fact
    /// worth knowing before trusting `carried_addons` to mean "will work".
    #[tokio::test]
    async fn a_custom_module_survives_a_real_migration() {
        if std::env::var("ORCHESTRATOR_TEST_REAL_UPGRADE").is_err() {
            eprintln!("skipping: set ORCHESTRATOR_TEST_REAL_UPGRADE=1 (needs two Odoo checkouts and several minutes)");
            return;
        }
        let Some((core, _server, dir)) = core_with_running_postgres_and_server().await else {
            panic!("this test was asked for explicitly, so a missing Postgres is a failure, not a skip");
        };
        eprintln!("[custom-migration] postgres up");
        core.with_environment(dir.path(), vec![]);

        let old = core.create_server("legacy-with-custom", "16.0", 8069, _server.postgres_instance_id).unwrap();
        eprintln!("[custom-migration] fetching Odoo 16…");
        let runtime = core.ensure_odoo("16.0").await.expect("Odoo 16 should be fetchable");
        core.create_addons_source(old.id, "core", runtime.checkout_root.join("addons").to_string_lossy(), SourceKind::Core, 0)
            .unwrap();
        core.create_addons_source(
            old.id,
            "base",
            runtime.checkout_root.join("odoo").join("addons").to_string_lossy(),
            SourceKind::Core,
            1,
        )
        .unwrap();

        // A minimal private module — just enough to prove the code has to
        // be found and re-initialized, not enough to need a real migration
        // script of its own.
        let addons_dir = tempfile::TempDir::new().unwrap();
        let module_dir = addons_dir.path().join("acme_custom");
        std::fs::create_dir_all(module_dir.join("models")).unwrap();
        std::fs::write(
            module_dir.join("__init__.py"),
            "from . import models\n",
        )
        .unwrap();
        // Deliberately *not* the `16.0.1.0.0` (series.major.minor.patch)
        // format almost every real OCA/custom module actually declares.
        // Odoo 17 validates a manifest's version against the *running*
        // series before it will even parse the module — `16.0.x.y.z` is
        // flatly rejected on a 17.0 Odoo, series-carried-along or not, with
        // "Invalid version ... Modules should have a version in format
        // `x.y`, `x.y.z`, `17.0.x.y` or `17.0.x.y.z`". A bare `x.y.z` is
        // the one form that survives any series unmodified, which is
        // exactly why it's used here rather than the realistic form — see
        // the doc comment this test is attached to and
        // `odoo-orchestrator-whats-next.md` for what this means for a real
        // client module.
        std::fs::write(
            module_dir.join("__manifest__.py"),
            "{'name': 'Acme Custom', 'version': '1.0.0', 'depends': ['base'], 'installable': True}\n",
        )
        .unwrap();
        std::fs::write(module_dir.join("models").join("__init__.py"), "from . import res_partner\n").unwrap();
        std::fs::write(
            module_dir.join("models").join("res_partner.py"),
            "from odoo import fields, models\n\n\nclass ResPartner(models.Model):\n    _inherit = 'res.partner'\n    x_acme_note = fields.Char()\n",
        )
        .unwrap();
        core.create_addons_source(old.id, "acme's own code", addons_dir.path().to_string_lossy(), SourceKind::Private, 2)
            .unwrap();

        let database = core.create_database(old.id, "acme_legacy_db").await.unwrap();
        eprintln!("[custom-migration] installing base + acme_custom on 16.0…");
        core.install_modules_here(database.id, &["base".to_string(), "acme_custom".to_string()])
            .await
            .expect("a real 16.0 database with a custom module installed");

        let conn = core.pg_conn_info_for_running_instance(old.postgres_instance_id).unwrap();
        pg_admin::execute(
            &conn,
            "acme_legacy_db",
            "INSERT INTO res_partner (name, active, company_id, x_acme_note) VALUES ('Acme Client', true, 1, 'do not lose me')",
        )
        .await
        .unwrap();

        let plan = core.plan_upgrade(database.id, "17.0").await.unwrap();
        assert_eq!(plan.carried_addons.len(), 1, "the private source should ride along; core must not be carried twice");
        assert_eq!(plan.carried_addons[0].label, "acme's own code");
        assert_eq!(plan.carried_addons[0].kind, SourceKind::Private);

        eprintln!("[custom-migration] running the migration…");
        let run = core.run_upgrade(database.id, "17.0").await.unwrap();
        eprintln!(
            "[custom-migration] run finished: completed {}, failed {:?}",
            run.completed.len(),
            run.failed.as_ref().map(|f| &f.message)
        );
        assert!(run.failed.is_none(), "migration with a carried custom module failed: {:?}", run.failed);

        let working = core.get_database(run.working_database_id.unwrap()).unwrap();
        let module_state = pg_admin::query_scalar(
            &conn,
            &working.name,
            "SELECT state FROM ir_module_module WHERE name = 'acme_custom'",
        )
        .await
        .unwrap();
        assert_eq!(
            module_state.as_deref(),
            Some("installed"),
            "the custom module must still be resolvable and installed after the migration, not missing or uninstalled"
        );

        let note = pg_admin::query_scalar(
            &conn,
            &working.name,
            "SELECT x_acme_note FROM res_partner WHERE name = 'Acme Client'",
        )
        .await
        .unwrap();
        assert_eq!(note.as_deref(), Some("do not lose me"), "the custom module's own column and data must survive");
    }

    /// The plan has to be honest about work it hasn't done yet.
    #[tokio::test]
    async fn a_plan_says_what_it_would_have_to_fetch_first() {
        let (core, pg_id, dir) = core_with_postgres_instance();
        core.with_environment(dir.path(), vec![]);
        let server = core.create_server("acme", "15.0", 8069, pg_id).unwrap();
        // A metadata row only: planning reads versions and paths, and
        // never touches Postgres, so this test needs no running cluster.
        let database = Database::new(server.id, "acme_db");
        core.db.insert_database(&database).unwrap();

        let plan = core.plan_upgrade(database.id, "18.0").await.unwrap();
        assert_eq!(plan.hops.len(), 3, "15 → 18 cannot be done in one step");
        assert_eq!(plan.hops.iter().map(|h| h.to.as_str()).collect::<Vec<_>>(), vec!["16.0", "17.0", "18.0"]);
        assert_eq!(plan.hops[0].from, "15.0");
        assert_eq!(plan.hops[1].from, "16.0");
        assert!(plan.hops.iter().all(|h| !h.openupgrade_ready), "nothing has been fetched in this test");
        // The copy, never the original.
        assert_eq!(plan.into, "acme_db_v180");
        assert_eq!(plan.database_name, "acme_db");
    }

    /// The fast, always-on half of the custom-module story: no real Odoo
    /// needed to prove the plan lists exactly the right sources — every
    /// Private/OCA one, and never Core, which every hop's own checkout
    /// already supplies.
    #[tokio::test]
    async fn a_plan_carries_private_and_oca_addons_but_never_core() {
        let (core, pg_id, dir) = core_with_postgres_instance();
        core.with_environment(dir.path(), vec![]);
        let server = core.create_server("acme", "16.0", 8069, pg_id).unwrap();
        core.create_addons_source(server.id, "odoo-core", "/tmp/odoo-core", SourceKind::Core, 0).unwrap();
        core.create_addons_source(server.id, "acme's own code", "/tmp/acme-private", SourceKind::Private, 1).unwrap();
        core.create_addons_source(server.id, "OCA mirror", "/tmp/acme-oca", SourceKind::Oca, 2).unwrap();
        let database = Database::new(server.id, "acme_db");
        core.db.insert_database(&database).unwrap();

        let plan = core.plan_upgrade(database.id, "17.0").await.unwrap();
        let labels: Vec<&str> = plan.carried_addons.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, vec!["acme's own code", "OCA mirror"], "core must never be carried — the hop's own checkout supplies that");
    }

    /// A server with nothing but core registered — the common case for a
    /// database nobody has pointed custom code at yet — carries nothing,
    /// and the plan should say so rather than silently omitting the field.
    #[tokio::test]
    async fn a_plan_with_no_custom_addons_carries_nothing() {
        let (core, pg_id, dir) = core_with_postgres_instance();
        core.with_environment(dir.path(), vec![]);
        let server = core.create_server("acme", "16.0", 8069, pg_id).unwrap();
        core.create_addons_source(server.id, "odoo-core", "/tmp/odoo-core", SourceKind::Core, 0).unwrap();
        let database = Database::new(server.id, "acme_db");
        core.db.insert_database(&database).unwrap();

        let plan = core.plan_upgrade(database.id, "17.0").await.unwrap();
        assert!(plan.carried_addons.is_empty());
    }

    /// Asking for something OpenUpgrade can't do must fail before anything
    /// is copied or downloaded.
    #[tokio::test]
    async fn an_impossible_migration_is_refused_up_front() {
        let (core, pg_id, dir) = core_with_postgres_instance();
        core.with_environment(dir.path(), vec![]);
        let server = core.create_server("acme", "17.0", 8069, pg_id).unwrap();
        let database = Database::new(server.id, "acme_db");
        core.db.insert_database(&database).unwrap();

        assert!(core.plan_upgrade(database.id, "16.0").await.is_err(), "downgrading is not a migration");
        assert!(core.plan_upgrade(database.id, "nonsense").await.is_err());

        // And a run of a no-op plan changes nothing rather than making a
        // pointless copy.
        let run = core.run_upgrade(database.id, "17.0").await.unwrap();
        assert!(run.working_database_id.is_none(), "already on 17.0, so there is nothing to copy");
        assert!(run.completed.is_empty());
    }

    // --- is my code actually live? ----------------------------------------

    /// The question the Apps screen used to have to dodge. Version
    /// comparison can't see an edit that didn't bump the manifest — which
    /// is every edit made while iterating.
    #[tokio::test]
    async fn editing_a_module_without_touching_its_version_still_shows_as_changed() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        let Some(checkout_root) = odoo_checkout_root_for_tests() else { return };
        let Some(venv_python) = std::env::var_os("ORCHESTRATOR_TEST_ODOO_VENV_PYTHON").map(std::path::PathBuf::from) else {
            eprintln!("skipping: set ORCHESTRATOR_TEST_ODOO_VENV_PYTHON to a provisioned venv");
            return;
        };
        core.with_environment(_dir.path(), vec![checkout_root.clone()]);
        core.create_addons_source(server.id, "odoo-core", checkout_root.join("addons").to_string_lossy(), SourceKind::Core, 0)
            .unwrap();

        // A module of our own, so the test can edit it.
        let mine = _dir.path().join("mine");
        std::fs::create_dir_all(mine.join("acme_note")).unwrap();
        std::fs::write(
            mine.join("acme_note/__manifest__.py"),
            "{'name': 'Acme Note', 'version': '1.0', 'depends': ['base'], 'installable': True}",
        )
        .unwrap();
        std::fs::write(mine.join("acme_note/__init__.py"), "").unwrap();
        core.create_addons_source(server.id, "mine", mine.to_string_lossy(), SourceKind::Private, 1).unwrap();

        let database = core.create_database(server.id, "freshness_db").await.unwrap();
        core.install_modules(database.id, &["acme_note".to_string()], &checkout_root, &venv_python, _dir.path().join("run"))
            .await
            .unwrap();

        let fresh = core.module_freshness(database.id).await.unwrap();
        let mine_now = fresh.iter().find(|m| m.technical_name == "acme_note").expect("it was just installed");
        assert_eq!(mine_now.freshness, Freshness::Live, "straight after installing, the code on disk is what's running");

        // Edit the Python. Deliberately do NOT bump the version — this is
        // exactly the case the old version-comparison could not see.
        std::fs::write(mine.join("acme_note/models.py"), "# a change nobody bumped the manifest for").unwrap();

        let fresh = core.module_freshness(database.id).await.unwrap();
        let mine_now = fresh.iter().find(|m| m.technical_name == "acme_note").unwrap();
        assert_eq!(mine_now.freshness, Freshness::Changed, "the edit has to be visible without a version bump");

        // Odoo's own modules were installed as dependencies, by this app,
        // so they're recorded and unchanged.
        let base = fresh.iter().find(|m| m.technical_name == "base").expect("base comes in as a dependency");
        assert_ne!(base.freshness, Freshness::Changed, "nothing touched Odoo's own code");

        // And updating it makes the answer true again.
        core.upgrade_modules(
            database.id,
            &["acme_note".to_string()],
            &checkout_root,
            &venv_python,
            _dir.path().join("run"),
        )
        .await
        .unwrap();
        let fresh = core.module_freshness(database.id).await.unwrap();
        assert_eq!(
            fresh.iter().find(|m| m.technical_name == "acme_note").unwrap().freshness,
            Freshness::Live,
            "after updating, the code on disk is live again",
        );
    }

    /// A module this app didn't install is *unknown*, not "fine". Reporting
    /// it as up to date would be the same lie in a new place.
    #[tokio::test]
    async fn a_module_installed_outside_this_app_reads_as_unknown() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        let Some(checkout_root) = odoo_checkout_root_for_tests() else { return };
        let Some(venv_python) = std::env::var_os("ORCHESTRATOR_TEST_ODOO_VENV_PYTHON").map(std::path::PathBuf::from) else {
            return;
        };
        core.create_addons_source(server.id, "odoo-core", checkout_root.join("addons").to_string_lossy(), SourceKind::Core, 0)
            .unwrap();
        let database = core.create_database(server.id, "outside_db").await.unwrap();
        core.install_modules(database.id, &["base".to_string()], &checkout_root, &venv_python, _dir.path().join("run"))
            .await
            .unwrap();

        // Forget what we recorded — the state of a database somebody set up
        // before this app ever saw it.
        core.db.forget_module_installs(database.id).unwrap();

        let fresh = core.module_freshness(database.id).await.unwrap();
        let counts: Vec<_> = fresh.iter().map(|m| (m.technical_name.clone(), m.freshness)).collect();
        assert!(
            fresh.iter().all(|m| matches!(m.freshness, Freshness::Unrecorded | Freshness::NotOnDisk)),
            "with nothing recorded, nothing may claim to be up to date: {counts:?}",
        );
        assert!(
            fresh.iter().any(|m| m.freshness == Freshness::Unrecorded),
            "and the modules that *are* on the addons path should say so",
        );

        // `base` comes back `NotOnDisk`, and that is correct rather than a
        // bug in this test: it lives in `odoo/addons`, which Odoo prepends
        // to the addons path itself and which this server has not been
        // told about. Registering a checkout should find that folder too —
        // see `discover_addons_roots`, which is what fixes it.
        assert_eq!(
            fresh.iter().find(|m| m.technical_name == "base").map(|m| m.freshness),
            Some(Freshness::NotOnDisk),
        );
    }

    // --- the template cache -----------------------------------------------

    /// The promise: the first database of a shape pays for a real Odoo
    /// boot, and every one after it is a copy. Measured, because "instant"
    /// is a claim about wall-clock time.
    #[tokio::test]
    async fn the_second_database_of_a_shape_is_a_copy_not_a_boot() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        let Some(checkout_root) = odoo_checkout_root_for_tests() else { return };
        let Some(venv_python) = std::env::var_os("ORCHESTRATOR_TEST_ODOO_VENV_PYTHON").map(std::path::PathBuf::from) else {
            eprintln!("skipping: set ORCHESTRATOR_TEST_ODOO_VENV_PYTHON to a provisioned venv");
            return;
        };
        core.with_environment(_dir.path(), vec![checkout_root.clone()]);
        core.create_addons_source(server.id, "odoo-core", checkout_root.join("addons").to_string_lossy(), SourceKind::Core, 0)
            .unwrap();
        let _ = venv_python;

        let modules = vec!["base".to_string()];

        let cold_started = std::time::Instant::now();
        let (first, from_cache) = core.create_database_with_modules(server.id, "first_db", &modules).await.unwrap();
        let cold = cold_started.elapsed();
        assert!(!from_cache, "there was no cache to hit on the first one");

        let warm_started = std::time::Instant::now();
        let (second, from_cache) = core.create_database_with_modules(server.id, "second_db", &modules).await.unwrap();
        let warm = warm_started.elapsed();
        assert!(from_cache, "the second one of the same shape must come from the cache");

        // The whole point, stated as the thing a person would notice.
        eprintln!("template cache: cold {cold:?}, warm {warm:?}");
        assert!(
            warm * 4 < cold,
            "the cached path should be dramatically faster: cold {cold:?}, warm {warm:?}",
        );

        // And it must be a real Odoo database, not just a fast empty one.
        let conn = core.pg_conn_info_for_running_instance(server.postgres_instance_id).unwrap();
        for name in [&first.name, &second.name] {
            let modules_installed = pg_admin::query_scalar(
                &conn,
                name,
                "SELECT count(*) FROM ir_module_module WHERE state = 'installed'",
            )
            .await
            .unwrap()
            .unwrap();
            assert!(modules_installed.parse::<i64>().unwrap() > 0, "{name} should be a working Odoo database");
        }

        // The cache is visible and disposable.
        let cached = core.cached_templates().await.unwrap();
        assert_eq!(cached.len(), 1, "one shape, one template");
        assert!(cached[0].size_bytes > 0);

        // And it isn't mistaken for a database somebody made.
        let reconciliation = core.reconcile_databases().await.unwrap();
        assert!(
            !reconciliation.untracked.iter().any(|d| d.name.starts_with("oo_template_")),
            "the app's own cache must not be reported as an unknown database",
        );

        let freed = core.clear_template_cache().await.unwrap();
        assert!(freed > 0);
        assert!(core.cached_templates().await.unwrap().is_empty());
    }

    /// The cache clear is classified as `system:write` rather than
    /// `databases:write` on the argument that it can only ever touch names
    /// this app built. That argument has to be true structurally, not by
    /// convention — so this is the test that it is.
    #[tokio::test]
    async fn clearing_the_cache_cannot_reach_a_database_somebody_made() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        let precious = core.create_database(server.id, "clients_live_work").await.unwrap();

        // A template-looking database, and a real one, side by side.
        let conn = core.pg_conn_info_for_running_instance(server.postgres_instance_id).unwrap();
        pg_admin::create_database(&conn, "oo_template_deadbeef00000000").await.unwrap();

        core.clear_template_cache().await.unwrap();

        assert!(
            pg_admin::database_exists(&conn, "clients_live_work").await.unwrap(),
            "clearing the cache must not be able to touch a database somebody made",
        );
        assert!(core.get_database(precious.id).is_ok(), "and its row must survive too");
        assert!(!pg_admin::database_exists(&conn, "oo_template_deadbeef00000000").await.unwrap());
    }

    /// Editing a module has to invalidate every template built from it,
    /// or the cache confidently hands back yesterday's schema.
    #[tokio::test]
    async fn changing_a_module_changes_the_template_it_would_be_cached_under() {
        let (core, pg_id, dir) = core_with_postgres_instance();
        let server = core.create_server("acme", "17.0", 8069, pg_id).unwrap();
        let addons = dir.path().join("addons");
        std::fs::create_dir_all(addons.join("acme_sales")).unwrap();
        std::fs::write(addons.join("acme_sales/__manifest__.py"), "{'name': 'Acme', 'version': '1.0'}").unwrap();
        core.create_addons_source(server.id, "mine", addons.to_string_lossy(), SourceKind::Private, 0).unwrap();

        let modules = vec!["acme_sales".to_string()];
        let before = core.template_key(server.id, &modules).unwrap();

        std::fs::write(addons.join("acme_sales/models.py"), "class Order: pass").unwrap();
        let after = core.template_key(server.id, &modules).unwrap();
        assert_ne!(before, after, "a module that changed cannot reuse a template built before the change");

        // And an unrelated edit elsewhere must not churn the key, or the
        // cache would never hit.
        std::fs::write(dir.path().join("unrelated.txt"), "nothing to do with modules").unwrap();
        assert_eq!(core.template_key(server.id, &modules).unwrap(), after);
    }

    // --- neutralization ---------------------------------------------------

    /// The whole point, against a real Odoo: a database carrying a working
    /// SMTP password and a dozen live crons must come out the other side
    /// unable to reach anybody.
    #[tokio::test]
    async fn neutralizing_a_restored_copy_really_disarms_it() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        let Some(checkout_root) = odoo_checkout_root_for_tests() else { return };
        let Some(venv_python) = std::env::var_os("ORCHESTRATOR_TEST_ODOO_VENV_PYTHON").map(std::path::PathBuf::from) else {
            eprintln!("skipping: set ORCHESTRATOR_TEST_ODOO_VENV_PYTHON to a provisioned venv");
            return;
        };
        core.create_addons_source(server.id, "odoo-core", checkout_root.join("addons").to_string_lossy(), SourceKind::Core, 0)
            .unwrap();

        let runtime_dir = _dir.path().join("run");
        let database = core.create_database(server.id, "prodcopy_test").await.unwrap();
        core.install_modules(database.id, &["base".to_string()], &checkout_root, &venv_python, &runtime_dir)
            .await
            .unwrap();

        let conn = {
            let server = core.get_server(database.server_id).unwrap();
            core.pg_conn_info_for_running_instance(server.postgres_instance_id).unwrap()
        };
        // Stand in for what a client's dump actually carries: a real
        // outgoing mail server with a real password on it.
        pg_admin::execute(
            &conn,
            "prodcopy_test",
            "INSERT INTO ir_mail_server (name, smtp_host, smtp_port, smtp_user, smtp_pass, active, smtp_encryption, smtp_authentication) \
             VALUES ('Client SMTP', 'smtp.client.example', 587, 'billing@client.example', 'hunter2', true, 'starttls', 'login')",
        )
        .await
        .unwrap();

        let crons_before: i64 = pg_admin::query_scalar(&conn, "prodcopy_test", "SELECT count(*) FROM ir_cron WHERE active")
            .await
            .unwrap()
            .unwrap()
            .parse()
            .unwrap();
        assert!(crons_before > 1, "a real Odoo database should have live crons to switch off");

        core.neutralize_database(database.id, &checkout_root, &venv_python, &runtime_dir).await.unwrap();

        let live_servers = pg_admin::query_scalar(
            &conn,
            "prodcopy_test",
            "SELECT count(*) FROM ir_mail_server WHERE active AND smtp_pass IS NOT NULL",
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(live_servers, "0", "a mail server with a working password must not survive neutralization");

        let crons_after: i64 = pg_admin::query_scalar(&conn, "prodcopy_test", "SELECT count(*) FROM ir_cron WHERE active")
            .await
            .unwrap()
            .unwrap()
            .parse()
            .unwrap();
        assert!(crons_after < crons_before, "{crons_before} crons before, {crons_after} after");

        // And the app reads the state back off the database, not off its
        // own memory of having done it.
        let states = core.neutralization_states().await.unwrap();
        let mine = states.iter().find(|(id, _)| *id == database.id).map(|(_, s)| *s);
        assert_eq!(mine, Some(NeutralizationState::Neutralized));
    }

    /// The three answers that aren't "neutralized" have to stay distinct.
    /// Reading "I couldn't check" as "it's fine" is how somebody emails a
    /// client's customer list from a laptop.
    #[tokio::test]
    async fn a_database_no_odoo_ever_touched_is_not_reported_as_dangerous() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        let database = core.create_database(server.id, "bare_db").await.unwrap();
        let states = core.neutralization_states().await.unwrap();
        assert_eq!(
            states.iter().find(|(id, _)| *id == database.id).map(|(_, s)| *s),
            Some(NeutralizationState::NotOdoo),
            "a bare Postgres database has nothing to neutralize and nothing to warn about",
        );
    }

    /// Provenance is what decides whether a copy deserves a warning, and it
    /// has to survive the trip through SQLite.
    #[tokio::test]
    async fn a_database_remembers_where_its_contents_came_from() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        let made = core.create_database(server.id, "made_here").await.unwrap();
        assert_eq!(made.origin, model::DatabaseOrigin::Created);
        assert!(!made.origin.may_hold_foreign_data());

        let copy = core.duplicate_database(made.id, "made_here_copy").await.unwrap();
        assert_eq!(copy.origin, model::DatabaseOrigin::Duplicated);

        let backups = tempfile::TempDir::new().unwrap();
        let dump = core.backup_database(made.id, backups.path()).await.unwrap();
        let restored = core.restore_database(server.id, "came_from_elsewhere", &dump).await.unwrap();
        assert_eq!(restored.origin, model::DatabaseOrigin::Restored);
        assert!(restored.origin.may_hold_foreign_data(), "a restored dump is the case this exists for");

        // Through the list, not just the return value — a different SELECT
        // is a second chance to drop the column.
        let listed = core.list_databases(Some(server.id)).unwrap();
        assert_eq!(listed.iter().find(|d| d.name == "came_from_elsewhere").unwrap().origin, model::DatabaseOrigin::Restored);
    }

    // --- reload on save --------------------------------------------------

    fn changed(paths: &[&str]) -> notify::Result<notify::Event> {
        Ok(notify::Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Data(notify::event::DataChange::Content)),
            paths: paths.iter().map(std::path::PathBuf::from).collect(),
            attrs: Default::default(),
        })
    }

    fn modules_for(paths: &[&str]) -> Vec<String> {
        let roots = vec![std::path::PathBuf::from("/code/addons"), std::path::PathBuf::from("/code/oca")];
        let mut found = std::collections::HashSet::new();
        collect_modules(&changed(paths), &roots, &mut found);
        let mut found: Vec<String> = found.into_iter().collect();
        found.sort();
        found
    }

    #[test]
    fn a_changed_data_file_names_the_module_it_belongs_to() {
        assert_eq!(modules_for(&["/code/addons/acme_sales/views/order.xml"]), vec!["acme_sales"]);
        // Any depth under the module, and any watched root.
        assert_eq!(modules_for(&["/code/oca/partner_firstname/data/a/b/c.csv"]), vec!["partner_firstname"]);
        // One event can carry several paths, and two files in the same
        // module must still mean one update, not two.
        assert_eq!(
            modules_for(&["/code/addons/acme_sales/views/a.xml", "/code/addons/acme_sales/views/b.xml"]),
            vec!["acme_sales"]
        );
    }

    #[test]
    fn python_changes_are_left_to_odoos_own_reloader() {
        // Running `-u` for a .py change as well would mean two restarts for
        // one save: Odoo's `--dev=reload` already handles this file.
        assert!(modules_for(&["/code/addons/acme_sales/models/order.py"]).is_empty());
    }

    #[test]
    fn editor_scratch_files_are_not_edits() {
        // Vim swap files, emacs autosaves and `.#` locks all land in the
        // same folder as the real file and would otherwise trigger a
        // database update every time somebody merely *opened* a view.
        assert!(modules_for(&["/code/addons/acme_sales/views/.order.xml.swp"]).is_empty());
        assert!(modules_for(&["/code/addons/acme_sales/views/#order.xml#"]).is_empty());
        assert!(modules_for(&["/code/addons/acme_sales/views/order.xml~"]).is_empty());
    }

    #[test]
    fn a_file_outside_every_watched_folder_belongs_to_no_module() {
        assert!(modules_for(&["/somewhere/else/thing/views/order.xml"]).is_empty());
    }

    #[test]
    fn reading_a_file_is_not_a_reason_to_update_anything() {
        let roots = vec![std::path::PathBuf::from("/code/addons")];
        let mut found = std::collections::HashSet::new();
        let access = Ok(notify::Event {
            kind: notify::EventKind::Access(notify::event::AccessKind::Read),
            paths: vec![std::path::PathBuf::from("/code/addons/acme_sales/views/order.xml")],
            attrs: Default::default(),
        });
        collect_modules(&access, &roots, &mut found);
        assert!(found.is_empty());
    }

    #[test]
    fn a_new_odoo_reloads_nothing_until_asked() {
        // Restarting somebody's Odoo, or writing to their database, because
        // they saved a file is exactly the kind of surprise this product
        // exists to remove. Both switches start off.
        let (core, pg_id, _dir) = core_with_postgres_instance();
        let server = core.create_server("Acme", "17.0", 8069, pg_id).unwrap();
        assert!(!server.reload.reload_python);
        assert!(!server.reload.update_on_data_change);
        // And it survives a round trip through SQLite.
        let read_back = core.get_server(server.id).unwrap();
        assert_eq!(read_back.reload, server.reload);
    }

    #[test]
    fn the_reload_policy_is_remembered() {
        let (core, pg_id, _dir) = core_with_postgres_instance();
        let server = core.create_server("Acme", "17.0", 8069, pg_id).unwrap();
        core.db
            .set_reload_policy(server.id, model::ReloadPolicy { reload_python: false, update_on_data_change: true })
            .unwrap();
        let read_back = core.get_server(server.id).unwrap();
        assert!(read_back.reload.update_on_data_change);
        assert!(!read_back.reload.reload_python, "one switch must not flip the other");
        // And it comes back the same way through the list, not just the
        // single-row read — two different SELECTs, two chances to forget a
        // column.
        let listed = core.list_servers().unwrap().into_iter().find(|s| s.id == server.id).unwrap();
        assert_eq!(listed.reload, read_back.reload);
    }

    /// The config panel and the config file have to come from one place.
    #[test]
    fn the_previewed_config_is_the_config_that_would_run() {
        let (core, pg_id, _dir) = core_with_postgres_instance();
        let server = core.create_server("Acme", "17.0", 8069, pg_id).unwrap();
        let before = core.preview_odoo_conf(server.id).unwrap();
        assert!(before.conf.contains("http_port = 8069"));
        assert!(before.conf.contains("dbfilter = ^%d$"));
        assert_eq!(before.command, "odoo-bin -c odoo.conf");

        core.db
            .set_reload_policy(server.id, model::ReloadPolicy { reload_python: true, update_on_data_change: false })
            .unwrap();
        let after = core.preview_odoo_conf(server.id).unwrap();
        // On the flags, never in the file: Odoo reads `dev_mode` from the
        // config and then overwrites it from `--dev`, so writing it into
        // `odoo.conf` would look like a working setting and do nothing.
        assert!(after.command.ends_with("--dev=reload,qweb,xml"), "{}", after.command);
        assert!(!after.conf.contains("dev_mode"), "the file must not pretend to carry this");
    }

    /// The switch has to *be* the watcher, not a row that describes one.
    #[tokio::test]
    async fn turning_data_updates_on_starts_a_real_watcher_and_off_stops_it() {
        let (core, pg_id, _dir) = core_with_postgres_instance();
        let server = core.create_server("Acme", "17.0", 8069, pg_id).unwrap();
        let addons = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(addons.path().join("acme_sales/views")).unwrap();
        core.create_addons_source(server.id, "mine", addons.path().to_str().unwrap(), SourceKind::Private, 0).unwrap();

        let watching = || core.data_watchers.lock().unwrap().contains_key(&server.id);
        assert!(!watching(), "nothing should be watched before anyone asks");

        core.set_reload_policy(server.id, model::ReloadPolicy { reload_python: false, update_on_data_change: true })
            .await
            .unwrap();
        assert!(watching(), "the setting is a live watcher or it is a lie");

        core.set_reload_policy(server.id, model::ReloadPolicy { reload_python: false, update_on_data_change: false })
            .await
            .unwrap();
        assert!(!watching(), "turning it off must stop watching, not just unset a flag");
    }

    /// The end of the chain that `collect_modules`'s unit tests can't reach:
    /// that `notify` really does deliver an event for a save under a watched
    /// addons folder, and that the event it delivers is one `collect_modules`
    /// recognises. Without this, every test above could pass while the
    /// feature watched nothing.
    #[test]
    fn saving_a_view_under_a_watched_folder_really_reaches_us() {
        use notify::Watcher;
        let addons = tempfile::TempDir::new().unwrap();
        let module = addons.path().join("acme_sales/views");
        std::fs::create_dir_all(&module).unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        let mut watcher = notify::recommended_watcher(tx).unwrap();
        watcher.watch(addons.path(), notify::RecursiveMode::Recursive).unwrap();

        std::fs::write(module.join("order.xml"), "<odoo/>").unwrap();

        let roots = vec![addons.path().to_path_buf()];
        let mut found = std::collections::HashSet::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while found.is_empty() && std::time::Instant::now() < deadline {
            match rx.recv_timeout(std::time::Duration::from_millis(500)) {
                Ok(event) => collect_modules(&event, &roots, &mut found),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => break,
            }
        }
        assert!(found.contains("acme_sales"), "a saved view produced no module to update: {found:?}");
    }

    // --- projects --------------------------------------------------------

    #[test]
    fn a_project_holds_odoos_and_reports_what_it_holds() {
        let (core, pg_id, _dir) = core_with_postgres_instance();
        let acme = core.create_project("Acme", 2).unwrap();
        let sandbox = core.create_project("Sandbox", 4).unwrap();

        core.create_server_in_project("Acme 17", "17.0", 8069, pg_id, Some(acme.id)).unwrap();
        core.create_server_in_project("Acme 16", "16.0", 8070, pg_id, Some(acme.id)).unwrap();
        core.create_server_in_project("Playground", "17.0", 8071, pg_id, Some(sandbox.id)).unwrap();

        let servers = core.list_servers().unwrap();
        let in_acme: Vec<_> = servers.iter().filter(|s| s.project_id == Some(acme.id)).collect();
        assert_eq!(in_acme.len(), 2, "both Acme Odoos should be filed under Acme");
        assert_eq!(servers.iter().filter(|s| s.project_id == Some(sandbox.id)).count(), 1);

        // The colour is stored, not derived — renaming must not re-colour it.
        let renamed = core.rename_project(acme.id, "Acme Retail", acme.color).unwrap();
        assert_eq!(renamed.name, "Acme Retail");
        assert_eq!(renamed.color, 2);
        assert_eq!(core.get_project(acme.id).unwrap().name, "Acme Retail");
    }

    #[test]
    fn deleting_a_project_that_still_holds_odoos_is_refused() {
        let (core, pg_id, _dir) = core_with_postgres_instance();
        let project = core.create_project("Acme", 0).unwrap();
        let server = core.create_server_in_project("Acme 17", "17.0", 8069, pg_id, Some(project.id)).unwrap();

        match core.delete_project(project.id) {
            Err(CoreError::ProjectNotEmpty { server_count, .. }) => assert_eq!(server_count, 1),
            other => panic!("expected ProjectNotEmpty, got {other:?}"),
        }
        // Still there — the refusal must not have half-deleted anything.
        assert!(core.get_project(project.id).is_ok());

        // Move the Odoo out, and the same delete now succeeds.
        let elsewhere = core.create_project("Elsewhere", 1).unwrap();
        core.move_server_to_project(server.id, elsewhere.id).unwrap();
        core.delete_project(project.id).unwrap();
        assert!(core.get_project(project.id).is_err());
        assert_eq!(core.get_server(server.id).unwrap().project_id, Some(elsewhere.id));
    }

    #[test]
    fn duplicating_a_project_copies_setup_on_free_ports_and_never_copies_databases() {
        let (core, pg_id, _dir) = core_with_postgres_instance();
        let source = core.create_project("Acme", 3).unwrap();
        let server = core.create_server_in_project("Acme 17", "17.0", 8069, pg_id, Some(source.id)).unwrap();
        core.create_addons_source(server.id, "Private", "/tmp/acme-addons", SourceKind::Private, 0).unwrap();
        core.create_addons_source(server.id, "OCA", "/tmp/oca", SourceKind::Oca, 1).unwrap();

        let copy = core.duplicate_project(source.id, "Acme (copy)").unwrap();
        assert_ne!(copy.id, source.id);

        let copied: Vec<_> = core.list_servers().unwrap().into_iter().filter(|s| s.project_id == Some(copy.id)).collect();
        assert_eq!(copied.len(), 1, "the Odoo definition should have been reproduced");
        assert_eq!(copied[0].odoo_version, "17.0");
        assert_ne!(copied[0].port, 8069, "a copy must not reuse a port another Odoo already claims");
        assert_eq!(copied[0].state, ServerState::Stopped);

        // Addons path came along, in the same load order — that's the part
        // that makes a duplicated project actually usable.
        let sources = core.list_addons_sources(copied[0].id).unwrap();
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].label, "Private");
        assert_eq!(sources[0].rank, 0);
        assert_eq!(sources[1].label, "OCA");

        // And the source project's own sources were not moved or mutated.
        assert_eq!(core.list_addons_sources(server.id).unwrap().len(), 2);
    }

    #[test]
    fn odoos_created_before_projects_existed_are_adopted_on_open() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("state.sqlite");

        // A server written the pre-projects way: no project_id at all.
        {
            let core = Core::open(&db_path).unwrap();
            let pg = core.create_postgres_instance("test", "16", 0, dir.path().to_str().unwrap()).unwrap();
            let server = core.create_server("Legacy", "16.0", 8069, pg.id).unwrap();
            assert_eq!(server.project_id, None, "this is the state being migrated from");
        }

        // Reopening adopts it, so the UI's top level can never show an
        // empty project list while servers exist underneath.
        let core = Core::open(&db_path).unwrap();
        let projects = core.list_projects().unwrap();
        assert_eq!(projects.len(), 1);
        let adopted = &core.list_servers().unwrap()[0];
        assert_eq!(adopted.project_id, Some(projects[0].id));

        // Idempotent: opening again must not create a second default project.
        let core = Core::open(&db_path).unwrap();
        assert_eq!(core.list_projects().unwrap().len(), 1);
    }

    /// Every `OdooServer` now needs a real `postgres_instance_id`
    /// (`create_server` validates it exists). Tests that only exercise the
    /// server/addons-source/module-scan surface don't need that instance
    /// *running* — `create_server` just checks the row exists — so this
    /// helper registers one without paying for a real subprocess start.
    fn core_with_postgres_instance() -> (Core, Uuid, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let core = Core::open(":memory:").expect("open in-memory db");
        let instance = core.create_postgres_instance("test", "16", 0, dir.path().to_str().unwrap()).unwrap();
        (core, instance.id, dir)
    }

    /// Whether two paths' metadata identifies the *same underlying file* on
    /// disk (the way two hardlinks to one inode do) — not merely equal
    /// content. There is no cross-platform std API for this: Unix exposes
    /// it as `ino()` (`std::os::unix::fs::MetadataExt`), Windows as
    /// `file_index()` (`std::os::windows::fs::MetadataExt`, which needs the
    /// file to actually be opened to populate, hence `File::open` here
    /// rather than `fs::metadata`). This was previously Unix-only code with
    /// no `#[cfg]` guard at all, which meant the whole crate failed to
    /// *compile* on Windows — caught by actually running CI there for the
    /// first time (windows-process-supervision.yml) rather than by review.
    #[cfg(unix)]
    fn same_file_identity(a: &std::path::Path, b: &std::path::Path) -> bool {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(a).unwrap().ino() == std::fs::metadata(b).unwrap().ino()
    }
    #[cfg(windows)]
    fn same_file_identity(a: &std::path::Path, b: &std::path::Path) -> bool {
        use std::os::windows::fs::MetadataExt;
        let fa = std::fs::File::open(a).unwrap().metadata().unwrap().file_index();
        let fb = std::fs::File::open(b).unwrap().metadata().unwrap().file_index();
        fa.is_some() && fa == fb
    }
    #[cfg(not(any(unix, windows)))]
    fn same_file_identity(_a: &std::path::Path, _b: &std::path::Path) -> bool {
        false
    }

    /// Database-lifecycle tests (`create_database`, `duplicate_database`,
    /// `drop_database`, `backup_database`, `restore_database`) do real
    /// Postgres I/O, so unlike the helper above they need an instance that
    /// is actually running. Returns `None` (callers should skip, matching
    /// the existing `two_distinct_postgres_instances_run_independently`
    /// pattern) when this environment has no system Postgres to start.
    async fn core_with_running_postgres_and_server() -> Option<(Core, OdooServer, tempfile::TempDir)> {
        if postgres::discover_system_bin_dir().is_none() {
            eprintln!("skipping: no system postgres installed in this environment");
            return None;
        }
        let dir = tempfile::TempDir::new().unwrap();
        let core = Core::open(":memory:").expect("open in-memory db");
        // Port 0 would ask the OS to pick one for the *listener*, but pg_ctl
        // needs a concrete port up front. Tests in this module run
        // concurrently in the same process, so a port derived only from the
        // pid collides across tests — use a per-call atomic counter instead
        // so every instance in this test binary gets a distinct port.
        // Asking the OS for a free port rather than counting up from a
        // fixed base. A cluster this suite leaves behind (a test that
        // panics before its `Core` drops, or a process that exits with one
        // still owned — destructors don't run then) keeps its port, and a
        // fixed base meant the *next* run of the suite collided with the
        // last one's leftovers. That cost two debugging detours before it
        // was worth fixing.
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("the OS should have a spare port");
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            port
        };
        let instance = core.create_postgres_instance("test", "16", port, dir.path().to_str().unwrap()).unwrap();
        core.start_postgres_instance(instance.id).await.expect("postgres instance should start");
        let server = core.create_server("acme", "17.0", 8069, instance.id).expect("create server");
        Some((core, server, dir))
    }

    #[test]
    fn create_server_persists_and_emits_event() {
        let (core, pg_id, _dir) = core_with_postgres_instance();
        // `subscribe` is called after the setup helper's `PostgresInstanceCreated`
        // was already broadcast, so this receiver only ever sees events from
        // this point forward — no draining needed.
        let mut events = core.subscribe();

        let server = core.create_server("acme", "17.0", 8069, pg_id).expect("create server");
        assert_eq!(server.odoo_version, "17.0");
        assert_eq!(server.postgres_instance_id, pg_id);

        let listed = core.list_servers().expect("list servers");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, server.id);

        let envelope = events.try_recv().expect("event was broadcast");
        match envelope.event {
            Event::ServerCreated { server_id, .. } => assert_eq!(server_id, server.id),
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn update_server_state_tracks_started_at_without_a_real_odoo_process() {
        let (core, pg_id, _dir) = core_with_postgres_instance();
        let server = core.create_server("acme", "17.0", 8069, pg_id).expect("create server");
        assert_eq!(server.started_at, None, "a freshly-created, never-started server has no uptime");

        core.db.update_server_state(server.id, &ServerState::Starting).unwrap();
        assert_eq!(core.get_server(server.id).unwrap().started_at, None, "Starting isn't Running yet");

        core.db.update_server_state(server.id, &ServerState::Running).unwrap();
        assert!(core.get_server(server.id).unwrap().started_at.is_some(), "Running must record when it started");

        core.db.update_server_state(server.id, &ServerState::Stopped).unwrap();
        assert_eq!(core.get_server(server.id).unwrap().started_at, None, "Stopped must clear uptime, not report stale data");

        let logged = core.recent_events(10).expect("recent events");
        assert_eq!(logged.len(), 2, "event log should be durable, not just broadcast");
    }

    #[test]
    fn create_server_rejects_unknown_postgres_instance() {
        let core = Core::open(":memory:").expect("open in-memory db");
        let err = core.create_server("acme", "17.0", 8069, Uuid::new_v4()).unwrap_err();
        assert!(matches!(err, CoreError::NotFound(_)));
    }

    #[tokio::test]
    async fn create_database_scopes_to_its_server() {
        let Some((core, server_a, _dir)) = core_with_running_postgres_and_server().await else { return };
        let server_b = core.create_server("northwind", "16.0", 8070, server_a.postgres_instance_id).unwrap();

        core.create_database(server_a.id, "acme").await.unwrap();
        core.create_database(server_a.id, "acme_test").await.unwrap();
        core.create_database(server_b.id, "northwind").await.unwrap();

        let a_dbs = core.list_databases(Some(server_a.id)).unwrap();
        let b_dbs = core.list_databases(Some(server_b.id)).unwrap();
        let all_dbs = core.list_databases(None).unwrap();

        assert_eq!(a_dbs.len(), 2);
        assert_eq!(b_dbs.len(), 1);
        assert_eq!(all_dbs.len(), 3);

        core.stop_postgres_instance(server_a.postgres_instance_id).await.unwrap();
    }

    #[tokio::test]
    async fn create_database_rejects_when_postgres_instance_not_running() {
        let (core, pg_id, _dir) = core_with_postgres_instance();
        let server = core.create_server("acme", "17.0", 8069, pg_id).unwrap();

        let err = core.create_database(server.id, "acme").await.unwrap_err();
        assert!(matches!(err, CoreError::PostgresInstanceNotRunning { .. }));
    }

    #[tokio::test]
    async fn duplicate_database_carries_over_real_data_via_core() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        let db = core.create_database(server.id, "acme").await.unwrap();

        let copy = core.duplicate_database(db.id, "acme_copy").await.unwrap();
        assert_eq!(copy.server_id, server.id);
        assert_ne!(copy.id, db.id);

        let all = core.list_databases(Some(server.id)).unwrap();
        assert_eq!(all.len(), 2, "both the source and the duplicate should be tracked");

        let events = core.recent_events(100).unwrap();
        assert!(events.iter().any(|e| matches!(e.event, Event::DatabaseDuplicated { database_id, .. } if database_id == copy.id)));

        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }

    // --- reconciliation, sizes, disk -------------------------------------

    #[tokio::test]
    async fn list_cluster_databases_reports_what_postgres_really_has() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        core.create_database(server.id, "acme").await.unwrap();

        let real = core.list_cluster_databases(server.postgres_instance_id).await.unwrap();
        assert!(real.iter().any(|d| d.name == "acme"), "the database just made should be listed: {real:?}");
        assert!(
            real.iter().all(|d| d.name != "template0" && d.name != "template1" && d.name != "postgres"),
            "template and maintenance databases are nobody's work and must not be listed: {real:?}"
        );
        assert!(real.iter().find(|d| d.name == "acme").unwrap().size_bytes > 0, "a real size should come back");

        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }

    /// Odoo cannot initialize a database on a non-UTF8 cluster: its very
    /// first insert into `ir_module_module` carries a `®` (from
    /// `delivery_mondialrelay`), and a SQL_ASCII database rejects it with
    /// `UntranslatableCharacter`. `initdb` with no `--encoding` inherits
    /// the machine's locale, so without the explicit flags this passes on
    /// a typical Mac and fails on a machine with an unset or C locale —
    /// the worst kind of bug, since it ships looking fine.
    ///
    /// This asserts the property directly rather than relying on the real
    /// Odoo tests to catch it, because those only run when someone has a
    /// checkout to point at, and this must hold on every machine.
    #[tokio::test]
    async fn every_database_this_app_makes_is_utf8_whatever_the_machine_locale() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        core.create_database(server.id, "encoding_check").await.unwrap();

        let conn = core.pg_conn_info_for_running_instance(server.postgres_instance_id).unwrap();
        let encoding = pg_admin::query_scalar(
            &conn,
            "postgres",
            "SELECT pg_encoding_to_char(encoding) FROM pg_database WHERE datname = 'encoding_check'",
        )
        .await
        .unwrap();
        assert_eq!(encoding.as_deref(), Some("UTF8"), "Odoo refuses to initialize anything but a UTF8 database");

        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }

    /// The whole point of the feature: a snapshot must capture the files
    /// as well as the rows, and a revert must put both back — **without
    /// anyone passing a filestore path**, because for months nobody did
    /// and every snapshot the app took was silently half a snapshot.
    ///
    /// Uses a real file in the real derived location, changes it, reverts,
    /// and asserts the original bytes are back. Attachments are what make
    /// an Odoo database somebody's actual work; rows alone are not it.
    #[tokio::test]
    async fn a_snapshot_captures_the_filestore_and_a_revert_restores_it() {
        let Some((core, server, dir)) = core_with_running_postgres_and_server().await else { return };
        core.with_environment(dir.path(), vec![]);
        let db = core.create_database(server.id, "acme").await.unwrap();

        // Exactly where a running Odoo would have put it. Odoo names
        // filestore files by their content hash and never edits one in
        // place — a changed attachment is a *new* file and the old one is
        // deleted — so that is what this simulates. (Hardlink snapshots
        // are only safe because of that property; `filestore.rs`'s module
        // doc explains it, and its own tests prove both halves.)
        let filestore = core.database_filestore_path(db.id).unwrap().expect("a data dir is configured");
        std::fs::create_dir_all(&filestore).unwrap();
        let original = filestore.join("a1b2c3");
        std::fs::write(&original, b"the original invoice").unwrap();

        // No filestore argument anywhere — this is the call the UI makes.
        let snapshot = core.create_snapshot(db.id, "before the mistake", None, None).await.unwrap();
        assert!(
            snapshot.filestore_snapshot_path.is_some(),
            "the snapshot must have taken the files too, without being asked: {snapshot:?}"
        );

        // The bad import: the attachment is replaced (delete + new hash).
        std::fs::remove_file(&original).unwrap();
        let replacement = filestore.join("d4e5f6");
        std::fs::write(&replacement, b"the wrong invoice").unwrap();

        core.revert_database_to_snapshot(db.id, snapshot.id, None, None).await.unwrap();

        assert_eq!(
            std::fs::read(&original).unwrap(),
            b"the original invoice",
            "reverting must bring the real file back, not just the database row pointing at it"
        );
        assert!(
            !replacement.exists(),
            "a file created after the snapshot must be gone — a revert is a restore, not a merge"
        );

        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }

    /// A backup must carry the attachments too, and a restore must put
    /// them back under the *new* database's name. Rows pointing at files
    /// that aren't there is not a restore.
    #[tokio::test]
    async fn a_backup_carries_the_filestore_and_a_restore_puts_it_back() {
        let Some((core, server, dir)) = core_with_running_postgres_and_server().await else { return };
        core.with_environment(dir.path(), vec![]);
        let db = core.create_database(server.id, "billing").await.unwrap();

        let filestore = core.database_filestore_path(db.id).unwrap().unwrap();
        std::fs::create_dir_all(&filestore).unwrap();
        std::fs::write(filestore.join("a1b2c3"), b"the invoice").unwrap();

        let backups = dir.path().join("backups");
        let dump = core.backup_database(db.id, &backups).await.unwrap();
        let backed_up_files = dump.with_extension("filestore");
        assert!(backed_up_files.is_dir(), "the backup must include the attachments, not just the rows");

        // Real bytes, not a hardlink: a backup has to survive its source
        // being deleted and to be movable to another disk. File identity
        // is a platform concept — inode on Unix, file index on Windows —
        // so this check dispatches to whichever the OS actually has rather
        // than only compiling on Unix (see `same_file_identity` below).
        let backed_up = backed_up_files.join("a1b2c3");
        let same_identity = same_file_identity(&backed_up, &filestore.join("a1b2c3"));
        assert!(!same_identity, "a backup must own its bytes, not share them with the live filestore");

        let events = core.recent_events(50).unwrap();
        assert!(
            events.iter().any(|e| matches!(&e.event, Event::DatabaseBackedUp { format, .. } if format == "dump+filestore")),
            "the event must say the files were included, so a reader knows without going to look"
        );

        let restored = core.restore_database(server.id, "billing_restored", &dump).await.unwrap();
        let restored_filestore = core.database_filestore_path(restored.id).unwrap().unwrap();
        assert_eq!(
            std::fs::read(restored_filestore.join("a1b2c3")).unwrap(),
            b"the invoice",
            "the restored database's attachments must be there, under its own name"
        );

        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }

    /// A database with no attachments backs up as a plain dump, and says
    /// so — rather than claiming a filestore it never had.
    #[tokio::test]
    async fn a_backup_of_a_database_with_no_attachments_says_dump_only() {
        let Some((core, server, dir)) = core_with_running_postgres_and_server().await else { return };
        core.with_environment(dir.path(), vec![]);
        let db = core.create_database(server.id, "plain").await.unwrap();

        let dump = core.backup_database(db.id, dir.path().join("backups")).await.unwrap();
        assert!(!dump.with_extension("filestore").exists());

        let events = core.recent_events(50).unwrap();
        assert!(events.iter().any(|e| matches!(&e.event, Event::DatabaseBackedUp { format, .. } if format == "dump")));

        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }

    /// A database that has never had an attachment has no filestore
    /// directory. That is an ordinary state — snapshotting it must
    /// succeed and say honestly that there were no files, not fail.
    #[tokio::test]
    async fn a_database_with_no_attachments_yet_snapshots_cleanly() {
        let Some((core, server, dir)) = core_with_running_postgres_and_server().await else { return };
        core.with_environment(dir.path(), vec![]);
        let db = core.create_database(server.id, "empty_one").await.unwrap();

        let snapshot = core.create_snapshot(db.id, "nothing to see", None, None).await.unwrap();
        assert!(snapshot.filestore_snapshot_path.is_none(), "no files exist, so the snapshot must say so rather than invent one");

        core.revert_database_to_snapshot(db.id, snapshot.id, None, None).await.unwrap();

        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }

    /// Dropping a database must take its attachments with it. Otherwise
    /// every dropped database leaves files on disk forever — invisible,
    /// counted by nothing, and eventually a real slice of the disk.
    #[tokio::test]
    async fn dropping_a_database_removes_its_filestore_too() {
        let Some((core, server, dir)) = core_with_running_postgres_and_server().await else { return };
        core.with_environment(dir.path(), vec![]);
        let db = core.create_database(server.id, "doomed").await.unwrap();

        let filestore = core.database_filestore_path(db.id).unwrap().unwrap();
        std::fs::create_dir_all(&filestore).unwrap();
        std::fs::write(filestore.join("attachment.bin"), b"bytes").unwrap();

        core.drop_database(db.id).await.unwrap();
        assert!(!filestore.exists(), "the filestore must go with the database, not be orphaned");

        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }

    #[tokio::test]
    async fn reconciliation_finds_a_database_made_outside_the_app() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        core.create_database(server.id, "acme").await.unwrap();

        // Made behind the app's back, exactly as `createdb` at a shell would.
        let conn = core.pg_conn_info_for_running_instance(server.postgres_instance_id).unwrap();
        pg_admin::create_database(&conn, "made_by_hand").await.unwrap();

        let report = core.reconcile_databases().await.unwrap();
        assert!(report.untracked.iter().any(|d| d.name == "made_by_hand"));
        assert!(!report.untracked.iter().any(|d| d.name == "acme"), "a tracked database is not a stray");
        assert!(report.missing.is_empty());
        assert!(report.unchecked.is_empty());

        // And adopting it makes the difference go away, without creating
        // anything: the row now points at the database that was already there.
        let adopted = core.adopt_database(server.id, "made_by_hand").await.unwrap();
        assert_eq!(adopted.name, "made_by_hand");
        let after = core.reconcile_databases().await.unwrap();
        assert!(after.untracked.is_empty(), "adopted databases stop being strays: {after:?}");

        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }

    #[tokio::test]
    async fn reconciliation_finds_a_row_whose_database_vanished_underneath_it() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        let db = core.create_database(server.id, "acme").await.unwrap();

        // Dropped behind the app's back — the app's row survives, wrongly.
        let conn = core.pg_conn_info_for_running_instance(server.postgres_instance_id).unwrap();
        pg_admin::drop_database(&conn, "acme").await.unwrap();

        let report = core.reconcile_databases().await.unwrap();
        assert_eq!(report.missing.len(), 1, "{report:?}");
        assert_eq!(report.missing[0].database_id, db.id);

        // Forgetting removes only the row. It must never be the thing that
        // drops a database — that's `drop_database`, deliberately separate.
        core.forget_database(db.id).unwrap();
        assert!(core.list_databases(None).unwrap().is_empty());
        assert!(core.reconcile_databases().await.unwrap().missing.is_empty());

        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }

    #[tokio::test]
    async fn a_store_that_cannot_be_asked_is_unchecked_not_missing() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        core.create_database(server.id, "acme").await.unwrap();
        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();

        let report = core.reconcile_databases().await.unwrap();
        assert_eq!(report.unchecked.len(), 1, "the stopped store should be reported as unasked: {report:?}");
        assert!(
            report.missing.is_empty(),
            "'I couldn't look' must never be reported as 'it's gone' — that's what sends someone to a backup they don't need"
        );
    }

    #[tokio::test]
    async fn adopting_a_database_that_does_not_exist_is_refused() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        let err = core.adopt_database(server.id, "never_existed").await.unwrap_err();
        assert!(matches!(err, CoreError::NotFound(_)), "got {err:?}");
        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }

    #[tokio::test]
    async fn refresh_database_sizes_writes_real_measured_sizes() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        let db = core.create_database(server.id, "acme").await.unwrap();
        assert_eq!(db.size_bytes, 0, "a fresh row starts unmeasured rather than guessing");

        let updated = core.refresh_database_sizes().await.unwrap();
        assert_eq!(updated, 1);
        assert!(core.get_database(db.id).unwrap().size_bytes > 0, "the size should now be Postgres's own answer");

        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }

    #[test]
    fn directory_size_counts_real_bytes_and_treats_a_missing_folder_as_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let nested = dir.path().join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("f.txt"), vec![0u8; 2048]).unwrap();
        std::fs::write(dir.path().join("g.txt"), vec![0u8; 1024]).unwrap();

        assert_eq!(odoo_runtime::directory_size(dir.path()), 3072);
        // A backups folder that doesn't exist yet means "no backups", which
        // is a legitimate state and not an error to propagate into stats.
        assert_eq!(odoo_runtime::directory_size(&dir.path().join("nope")), 0);
    }

    #[test]
    fn the_odoo_cache_refuses_to_delete_anything_outside_itself() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = odoo_runtime::RuntimeStore::new(dir.path().join("cache"));
        std::fs::create_dir_all(dir.path().join("cache")).unwrap();

        // Somebody's own checkout, sitting next to the cache rather than in it.
        let theirs = dir.path().join("their-odoo");
        std::fs::create_dir_all(theirs.join("odoo")).unwrap();
        std::fs::write(theirs.join("odoo-bin"), "#!/usr/bin/env python3").unwrap();
        std::fs::write(theirs.join("odoo").join("release.py"), "version_info = (17, 0, 0, 'final', 0, '')").unwrap();

        let err = store.remove_cached("../their-odoo").unwrap_err();
        assert!(matches!(err, odoo_runtime::OdooRuntimeError::NotInCache { .. }), "got {err:?}");
        assert!(theirs.exists(), "a checkout the user owns must survive a cache clear");
    }

    #[tokio::test]
    async fn drop_database_removes_it_from_postgres_and_sqlite() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        let db = core.create_database(server.id, "acme").await.unwrap();

        core.drop_database(db.id).await.unwrap();

        let remaining = core.list_databases(Some(server.id)).unwrap();
        assert!(remaining.is_empty());
        let err = core.get_database(db.id).unwrap_err();
        assert!(matches!(err, CoreError::NotFound(_)));

        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }

    #[tokio::test]
    async fn backup_and_restore_a_database_round_trips_through_core() {
        let Some((core, server, dir)) = core_with_running_postgres_and_server().await else { return };
        let db = core.create_database(server.id, "acme").await.unwrap();

        let backups_dir = dir.path().join("backups");
        let dump_path = core.backup_database(db.id, &backups_dir).await.unwrap();
        assert!(dump_path.exists(), "backup_database should actually write a dump file");

        let fetched = core.get_database(db.id).unwrap();
        assert!(fetched.last_backup_at.is_some(), "backup should be recorded on the database row");

        let restored = core.restore_database(server.id, "acme_restored", &dump_path).await.unwrap();
        assert_eq!(restored.server_id, server.id);
        assert_ne!(restored.id, db.id);

        let all = core.list_databases(Some(server.id)).unwrap();
        assert_eq!(all.len(), 2, "original plus the restored copy");

        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }

    // --- snapshots (task 3.1) --------------------------------------------

    /// Runs `psql -h <socket dir> -p <port> -U postgres -tAc <sql> <database>`
    /// directly — the same minimal escape hatch `pg_admin.rs`'s own test
    /// module uses, kept local here rather than exported, since it exists
    /// purely to let tests observe real Postgres state `Core`'s own API
    /// doesn't (and shouldn't) expose.
    fn psql_exec(bin_dir: &std::path::Path, data_dir: &std::path::Path, port: u16, database: &str, sql: &str) -> String {
        let output = std::process::Command::new(bin_dir.join("psql"))
            .arg("-h").arg(data_dir).arg("-p").arg(port.to_string()).arg("-U").arg("postgres")
            .arg(database).arg("-tAc").arg(sql)
            .output()
            .expect("psql should run");
        assert!(output.status.success(), "psql failed: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn database_exists_via_psql(bin_dir: &std::path::Path, data_dir: &std::path::Path, port: u16, name: &str) -> bool {
        psql_exec(bin_dir, data_dir, port, "postgres", &format!("SELECT 1 FROM pg_database WHERE datname = '{name}'")) == "1"
    }

    #[tokio::test]
    async fn create_snapshot_duplicates_real_data_database_only() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        let instance = core.get_postgres_instance(server.postgres_instance_id).unwrap();
        let bin_dir = postgres::discover_system_bin_dir().unwrap();

        let db = core.create_database(server.id, "acme").await.unwrap();
        psql_exec(&bin_dir, std::path::Path::new(&instance.data_dir), instance.port, &db.name, "CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('pre-migration state')");

        let snapshot = core.create_snapshot(db.id, "pre-migration", None, None).await.unwrap();
        assert_eq!(snapshot.database_id, db.id);
        assert_eq!(snapshot.server_id, server.id);
        assert_eq!(snapshot.name, "pre-migration");
        assert!(snapshot.filestore_snapshot_path.is_none(), "no filestore source was given, so this must be Postgres-only");
        assert_ne!(snapshot.snapshot_database_name, db.name, "the snapshot must live in its own database, not alias the source");

        let value = psql_exec(&bin_dir, std::path::Path::new(&instance.data_dir), instance.port, &snapshot.snapshot_database_name, "SELECT v FROM t");
        assert_eq!(value, "pre-migration state", "the snapshot database must have the source's real data at the moment it was taken");

        // Mutating the source afterward must not affect the already-taken snapshot.
        psql_exec(&bin_dir, std::path::Path::new(&instance.data_dir), instance.port, &db.name, "UPDATE t SET v = 'post-migration state'");
        let snapshot_value_after = psql_exec(&bin_dir, std::path::Path::new(&instance.data_dir), instance.port, &snapshot.snapshot_database_name, "SELECT v FROM t");
        assert_eq!(snapshot_value_after, "pre-migration state", "a snapshot must be frozen at the moment it was taken, not track the live database");

        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }

    #[tokio::test]
    async fn create_snapshot_with_filestore_hardlinks_real_files() {
        let Some((core, server, dir)) = core_with_running_postgres_and_server().await else { return };
        let db = core.create_database(server.id, "acme").await.unwrap();

        let filestore_source = dir.path().join("filestore");
        std::fs::create_dir_all(filestore_source.join("ab")).unwrap();
        std::fs::write(filestore_source.join("ab").join("abc123"), b"attachment bytes").unwrap();
        let snapshots_dir = dir.path().join("snapshots");

        let snapshot = core.create_snapshot(db.id, "with-filestore", Some(filestore_source), Some(snapshots_dir)).await.unwrap();
        let filestore_path = snapshot.filestore_snapshot_path.clone().expect("a filestore source was given, so this must be recorded");
        let copied = std::fs::read(std::path::Path::new(&filestore_path).join("ab").join("abc123")).unwrap();
        assert_eq!(copied, b"attachment bytes");

        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }

    #[tokio::test]
    async fn delete_snapshot_removes_its_database_and_filestore_copy() {
        let Some((core, server, dir)) = core_with_running_postgres_and_server().await else { return };
        let instance = core.get_postgres_instance(server.postgres_instance_id).unwrap();
        let bin_dir = postgres::discover_system_bin_dir().unwrap();
        let db = core.create_database(server.id, "acme").await.unwrap();

        let filestore_source = dir.path().join("filestore");
        std::fs::create_dir_all(&filestore_source).unwrap();
        std::fs::write(filestore_source.join("f1"), b"data").unwrap();
        let snapshots_dir = dir.path().join("snapshots");

        let snapshot = core.create_snapshot(db.id, "to-delete", Some(filestore_source), Some(snapshots_dir)).await.unwrap();
        let filestore_path = std::path::PathBuf::from(snapshot.filestore_snapshot_path.clone().unwrap());
        assert!(database_exists_via_psql(&bin_dir, std::path::Path::new(&instance.data_dir), instance.port, &snapshot.snapshot_database_name));
        assert!(filestore_path.exists());

        core.delete_snapshot(snapshot.id).await.unwrap();

        assert!(!database_exists_via_psql(&bin_dir, std::path::Path::new(&instance.data_dir), instance.port, &snapshot.snapshot_database_name), "the snapshot's real Postgres database must actually be dropped");
        assert!(!filestore_path.exists(), "the snapshot's filestore copy must actually be removed");
        let err = core.get_snapshot(snapshot.id).unwrap_err();
        assert!(matches!(err, CoreError::NotFound(_)));

        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }

    /// The whole point of storing `server_id` directly on `Snapshot`
    /// (rather than only `database_id`) — a snapshot must outlive the
    /// database it was taken from. This proves it: drop the source
    /// database entirely, then confirm the snapshot is still listable and
    /// still cleanly deletable.
    #[tokio::test]
    async fn snapshot_survives_and_stays_deletable_after_its_source_database_is_dropped() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        let instance = core.get_postgres_instance(server.postgres_instance_id).unwrap();
        let bin_dir = postgres::discover_system_bin_dir().unwrap();
        let db = core.create_database(server.id, "acme").await.unwrap();
        let snapshot = core.create_snapshot(db.id, "outlives-source", None, None).await.unwrap();

        core.drop_database(db.id).await.unwrap();
        let err = core.get_database(db.id).unwrap_err();
        assert!(matches!(err, CoreError::NotFound(_)), "sanity check: the source database really is gone");

        // The snapshot must still be listable and its own database untouched.
        let listed = core.list_snapshots(None).unwrap();
        assert!(listed.iter().any(|s| s.id == snapshot.id));
        assert!(database_exists_via_psql(&bin_dir, std::path::Path::new(&instance.data_dir), instance.port, &snapshot.snapshot_database_name));

        // And it must still be cleanly deletable, even with its source gone.
        core.delete_snapshot(snapshot.id).await.unwrap();
        assert!(!database_exists_via_psql(&bin_dir, std::path::Path::new(&instance.data_dir), instance.port, &snapshot.snapshot_database_name));

        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }

    #[tokio::test]
    async fn list_snapshots_scopes_to_database_when_asked() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        let db_a = core.create_database(server.id, "acme").await.unwrap();
        let db_b = core.create_database(server.id, "northwind").await.unwrap();

        core.create_snapshot(db_a.id, "snap-a1", None, None).await.unwrap();
        core.create_snapshot(db_a.id, "snap-a2", None, None).await.unwrap();
        core.create_snapshot(db_b.id, "snap-b1", None, None).await.unwrap();

        assert_eq!(core.list_snapshots(Some(db_a.id)).unwrap().len(), 2);
        assert_eq!(core.list_snapshots(Some(db_b.id)).unwrap().len(), 1);
        assert_eq!(core.list_snapshots(None).unwrap().len(), 3);

        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }

    #[tokio::test]
    async fn create_snapshot_emits_event_with_filestore_flag() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        let db = core.create_database(server.id, "acme").await.unwrap();

        let snapshot = core.create_snapshot(db.id, "flagged", None, None).await.unwrap();

        let events = core.recent_events(100).unwrap();
        let found = events.iter().find_map(|e| match &e.event {
            Event::SnapshotCreated { snapshot_id, has_filestore, .. } if *snapshot_id == snapshot.id => Some(*has_filestore),
            _ => None,
        });
        assert_eq!(found, Some(false), "no filestore source was given, so the event must say so");

        core.delete_snapshot(snapshot.id).await.unwrap();
        let events = core.recent_events(100).unwrap();
        assert!(events.iter().any(|e| matches!(e.event, Event::SnapshotDeleted { snapshot_id } if snapshot_id == snapshot.id)));

        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }

    // --- revert to snapshot (task 3.3) -----------------------------------

    /// The headline claim: reverting actually overwrites the *live*
    /// database's real data with the snapshot's frozen data, in place —
    /// same `Database` id and name throughout, only the Postgres contents
    /// underneath change.
    #[tokio::test]
    async fn revert_database_to_snapshot_restores_real_data_in_place() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        let instance = core.get_postgres_instance(server.postgres_instance_id).unwrap();
        let bin_dir = postgres::discover_system_bin_dir().unwrap();
        let db = core.create_database(server.id, "acme").await.unwrap();
        psql_exec(&bin_dir, std::path::Path::new(&instance.data_dir), instance.port, &db.name, "CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('pre-migration state')");

        let snapshot = core.create_snapshot(db.id, "pre-migration", None, None).await.unwrap();

        // Mutate the live database after the snapshot was taken.
        psql_exec(&bin_dir, std::path::Path::new(&instance.data_dir), instance.port, &db.name, "UPDATE t SET v = 'post-migration state'");
        let live_value = psql_exec(&bin_dir, std::path::Path::new(&instance.data_dir), instance.port, &db.name, "SELECT v FROM t");
        assert_eq!(live_value, "post-migration state");

        let counter = core.revert_database_to_snapshot(db.id, snapshot.id, None, None).await.unwrap();
        assert!(counter.is_none(), "no counter-snapshot name was given");

        // The database row itself is unchanged (same id, same name) — only
        // its real Postgres contents were reverted.
        let fetched = core.get_database(db.id).unwrap();
        assert_eq!(fetched.id, db.id);
        assert_eq!(fetched.name, db.name);

        let reverted_value = psql_exec(&bin_dir, std::path::Path::new(&instance.data_dir), instance.port, &db.name, "SELECT v FROM t");
        assert_eq!(reverted_value, "pre-migration state", "revert must actually overwrite the live database with the snapshot's frozen data");

        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }

    /// The friction ladder's "counter-snapshot offered" element for this
    /// rung, proven for real: a counter-snapshot taken during revert must
    /// actually contain the *pre-revert* (live) data, not the snapshot
    /// being reverted to — otherwise it would be useless as an undo for the
    /// revert itself.
    #[tokio::test]
    async fn revert_database_to_snapshot_takes_a_real_counter_snapshot_when_asked() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        let instance = core.get_postgres_instance(server.postgres_instance_id).unwrap();
        let bin_dir = postgres::discover_system_bin_dir().unwrap();
        let db = core.create_database(server.id, "acme").await.unwrap();
        psql_exec(&bin_dir, std::path::Path::new(&instance.data_dir), instance.port, &db.name, "CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('v1')");
        let snapshot = core.create_snapshot(db.id, "v1", None, None).await.unwrap();
        psql_exec(&bin_dir, std::path::Path::new(&instance.data_dir), instance.port, &db.name, "UPDATE t SET v = 'v2'");

        let counter = core
            .revert_database_to_snapshot(db.id, snapshot.id, Some("pre-revert-safety".to_string()), None)
            .await
            .unwrap()
            .expect("a counter-snapshot name was given");
        assert_eq!(counter.name, "pre-revert-safety");

        let counter_value = psql_exec(&bin_dir, std::path::Path::new(&instance.data_dir), instance.port, &counter.snapshot_database_name, "SELECT v FROM t");
        assert_eq!(counter_value, "v2", "the counter-snapshot must capture the live state as it was right before the revert, not the target snapshot's state");

        let listed = core.list_snapshots(Some(db.id)).unwrap();
        assert!(listed.iter().any(|s| s.id == counter.id), "the counter-snapshot must be a real, listable snapshot like any other");

        let events = core.recent_events(100).unwrap();
        assert!(events.iter().any(|e| matches!(&e.event, Event::DatabaseRevertedToSnapshot { database_id, snapshot_id, counter_snapshot_id } if *database_id == db.id && *snapshot_id == snapshot.id && *counter_snapshot_id == Some(counter.id))));

        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }

    #[tokio::test]
    async fn revert_database_to_snapshot_rejects_a_snapshot_from_a_different_database() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        let db_a = core.create_database(server.id, "acme").await.unwrap();
        let db_b = core.create_database(server.id, "northwind").await.unwrap();
        let snapshot_of_b = core.create_snapshot(db_b.id, "snap-b", None, None).await.unwrap();

        let err = core.revert_database_to_snapshot(db_a.id, snapshot_of_b.id, None, None).await.unwrap_err();
        assert!(matches!(err, CoreError::SnapshotNotForDatabase { snapshot_id, database_id } if snapshot_id == snapshot_of_b.id && database_id == db_a.id));

        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }

    #[tokio::test]
    async fn revert_database_to_snapshot_with_filestore_hardlinks_the_snapshots_files_into_target() {
        let Some((core, server, dir)) = core_with_running_postgres_and_server().await else { return };
        let db = core.create_database(server.id, "acme").await.unwrap();

        let filestore_source = dir.path().join("filestore");
        std::fs::create_dir_all(&filestore_source).unwrap();
        std::fs::write(filestore_source.join("f1"), b"snapshot bytes").unwrap();
        let snapshots_dir = dir.path().join("snapshots");
        let snapshot = core.create_snapshot(db.id, "with-filestore", Some(filestore_source), Some(snapshots_dir)).await.unwrap();

        // The "live" filestore target already has different content that
        // must be replaced by the revert, not merged with.
        let live_filestore = dir.path().join("live-filestore");
        std::fs::create_dir_all(&live_filestore).unwrap();
        std::fs::write(live_filestore.join("stale"), b"stale live bytes").unwrap();

        core.revert_database_to_snapshot(db.id, snapshot.id, None, Some(live_filestore.clone())).await.unwrap();

        assert!(!live_filestore.join("stale").exists(), "the pre-revert live filestore content must actually be replaced");
        assert_eq!(std::fs::read(live_filestore.join("f1")).unwrap(), b"snapshot bytes");

        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }

    #[tokio::test]
    async fn revert_database_to_snapshot_rejects_filestore_target_when_snapshot_has_none() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        let db = core.create_database(server.id, "acme").await.unwrap();
        let snapshot = core.create_snapshot(db.id, "db-only", None, None).await.unwrap();

        let err = core
            .revert_database_to_snapshot(db.id, snapshot.id, None, Some(std::path::PathBuf::from("/tmp/wont-be-used")))
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::SnapshotHasNoFilestore { snapshot_id } if snapshot_id == snapshot.id));

        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }

    // --- git-branch binding (task 3.2) ------------------------------------

    /// Real `git init` + one real commit in a tempdir — mirrors `git.rs`'s
    /// own test helper (kept local here rather than shared, same posture as
    /// `pg_admin.rs`'s test helpers being duplicated into `lib.rs`'s tests
    /// where `Core`-level behavior needs to observe real external state).
    fn real_git_repo(branch: &str) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git").arg("-C").arg(dir.path()).args(args).status().unwrap();
            assert!(status.success(), "git {args:?} failed");
        };
        run(&["init", "-q", "-b", branch]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "test"]);
        std::fs::write(dir.path().join("f"), b"hello").unwrap();
        run(&["add", "f"]);
        run(&["-c", "commit.gpgsign=false", "commit", "-q", "-m", "init"]);
        dir
    }

    #[test]
    fn current_git_branch_returns_none_when_no_private_addons_source_registered() {
        let (core, pg_id, _dir) = core_with_postgres_instance();
        let server = core.create_server("acme", "17.0", 8069, pg_id).unwrap();
        core.create_addons_source(server.id, "oca-only", "/tmp/oca", SourceKind::Oca, 0).unwrap();

        assert_eq!(core.current_git_branch(server.id).unwrap(), None, "no Private-kind source means nothing to suggest, not an error");
    }

    #[test]
    fn current_git_branch_returns_the_real_branch_of_the_linked_private_repo() {
        let (core, pg_id, _dir) = core_with_postgres_instance();
        let server = core.create_server("acme", "17.0", 8069, pg_id).unwrap();
        let repo = real_git_repo("main");
        std::process::Command::new("git").arg("-C").arg(repo.path()).args(["checkout", "-q", "-b", "feature/new-invoicing"]).status().unwrap();
        core.create_addons_source(server.id, "this repo", repo.path().to_str().unwrap(), SourceKind::Private, 0).unwrap();

        assert_eq!(core.current_git_branch(server.id).unwrap(), Some("feature/new-invoicing".to_string()));
    }

    /// A snapshot records the branch it was taken on, and the app can find
    /// its way back from a branch to the database state that belonged to
    /// it. Real git, real branches, real Postgres.
    #[tokio::test]
    async fn snapshots_remember_their_branch_and_can_be_found_from_it() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        let repo = real_git_repo("main");
        core.create_addons_source(server.id, "this repo", repo.path().to_str().unwrap(), SourceKind::Private, 0).unwrap();
        let db = core.create_database(server.id, "acme").await.unwrap();

        let on_main = core.create_snapshot(db.id, "before the refactor", None, None).await.unwrap();
        assert_eq!(on_main.git_branch.as_deref(), Some("main"), "a snapshot must record the branch it was taken on");

        std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["checkout", "-q", "-b", "feature/invoicing"])
            .status()
            .unwrap();
        let on_feature = core.create_snapshot(db.id, "invoicing wip", None, None).await.unwrap();
        assert_eq!(on_feature.git_branch.as_deref(), Some("feature/invoicing"));

        // The point of recording it: from a branch, find its snapshots.
        let feature_snapshots = core.snapshots_on_branch(server.id, "feature/invoicing").unwrap();
        assert_eq!(feature_snapshots.len(), 1, "{feature_snapshots:?}");
        assert_eq!(feature_snapshots[0].id, on_feature.id);

        let main_snapshots = core.snapshots_on_branch(server.id, "main").unwrap();
        assert_eq!(main_snapshots.len(), 1);
        assert_eq!(main_snapshots[0].id, on_main.id, "a branch must not pick up another branch's snapshots");

        assert!(
            core.snapshots_on_branch(server.id, "feature/never-existed").unwrap().is_empty(),
            "a branch with no snapshots is an ordinary state, not an error"
        );

        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }

    /// No linked repository means no branch — recorded as `None` rather
    /// than a guess, so nothing downstream can mistake a label for
    /// evidence.
    #[tokio::test]
    async fn a_snapshot_taken_with_no_linked_repo_records_no_branch() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        let db = core.create_database(server.id, "acme").await.unwrap();

        let snapshot = core.create_snapshot(db.id, "main", None, None).await.unwrap();
        assert_eq!(
            snapshot.git_branch, None,
            "the label happens to look like a branch name, which is not evidence that it is one"
        );
        assert!(core.snapshots_on_branch(server.id, "main").unwrap().is_empty());

        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }

    #[test]
    fn current_git_branch_returns_none_when_private_source_is_not_a_git_repo() {
        let (core, pg_id, _dir) = core_with_postgres_instance();
        let server = core.create_server("acme", "17.0", 8069, pg_id).unwrap();
        let plain_dir = tempfile::TempDir::new().unwrap();
        core.create_addons_source(server.id, "this repo", plain_dir.path().to_str().unwrap(), SourceKind::Private, 0).unwrap();

        assert_eq!(core.current_git_branch(server.id).unwrap(), None, "a real path that isn't a git repo must fail soft, not error out");
    }

    #[test]
    fn current_git_branch_ignores_non_private_sources_even_if_they_are_git_repos() {
        let (core, pg_id, _dir) = core_with_postgres_instance();
        let server = core.create_server("acme", "17.0", 8069, pg_id).unwrap();
        let repo = real_git_repo("vendor-branch");
        core.create_addons_source(server.id, "oca vendor repo", repo.path().to_str().unwrap(), SourceKind::Oca, 0).unwrap();

        assert_eq!(core.current_git_branch(server.id).unwrap(), None, "an OCA/vendor source being a git repo is irrelevant — only Private counts");
    }

    #[test]
    fn current_git_branch_rejects_unknown_server() {
        let core = Core::open(":memory:").expect("open in-memory db");
        let err = core.current_git_branch(Uuid::new_v4()).unwrap_err();
        assert!(matches!(err, CoreError::NotFound(_)));
    }

    #[test]
    fn shadowing_source_kind_round_trips_through_sqlite() {
        let (core, pg_id, _dir) = core_with_postgres_instance();
        let server = core.create_server("acme", "17.0", 8069, pg_id).unwrap();

        let created = core
            .create_addons_source(server.id, "this repo", "~/repos/acme-odoo/addons", SourceKind::Private, 0)
            .unwrap();

        let listed = core.list_addons_sources(server.id).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, created.id);
        assert_eq!(listed[0].kind, SourceKind::Private);
        assert_eq!(listed[0].rank, 0);
    }

    #[test]
    fn create_addons_source_rejects_unknown_server() {
        let core = Core::open(":memory:").expect("open in-memory db");
        let err = core.create_addons_source(Uuid::new_v4(), "orphan", "/tmp", SourceKind::Private, 0).unwrap_err();
        assert!(matches!(err, CoreError::NotFound(_)));
    }

    #[test]
    fn list_addons_sources_orders_by_rank() {
        let (core, pg_id, _dir) = core_with_postgres_instance();
        let server = core.create_server("acme", "17.0", 8069, pg_id).unwrap();
        core.create_addons_source(server.id, "core", "/tmp/core", SourceKind::Core, 2).unwrap();
        core.create_addons_source(server.id, "private", "/tmp/private", SourceKind::Private, 0).unwrap();
        core.create_addons_source(server.id, "oca", "/tmp/oca", SourceKind::Oca, 1).unwrap();

        let listed = core.list_addons_sources(server.id).unwrap();
        let labels: Vec<&str> = listed.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, vec!["private", "oca", "core"], "rank 0 (loads first) should come first");
    }

    #[test]
    fn scan_modules_emits_an_event_and_rejects_unknown_server() {
        let (core, pg_id, _dir) = core_with_postgres_instance();
        let mut events = core.subscribe();
        let server = core.create_server("acme", "17.0", 8069, pg_id).unwrap();

        let scan = core.scan_modules(server.id).unwrap();
        assert_eq!(scan.server_id, server.id);
        assert!(scan.modules.is_empty(), "no addons sources registered yet");

        let envelope = events.try_recv().expect("ServerCreated");
        assert!(matches!(envelope.event, Event::ServerCreated { .. }));
        let envelope = events.try_recv().expect("ModulesScanned should follow");
        assert!(matches!(envelope.event, Event::ModulesScanned { module_count: 0, shadow_count: 0, .. }));

        let err = core.scan_modules(Uuid::new_v4()).unwrap_err();
        assert!(matches!(err, CoreError::NotFound(_)));
    }

    // --- postgres instances --------------------------------------------

    #[test]
    fn create_postgres_instance_persists_and_emits_event() {
        let core = Core::open(":memory:").expect("open in-memory db");
        let mut events = core.subscribe();

        let instance = core.create_postgres_instance("main", "16", 55420, "/tmp/pg-main").unwrap();
        assert_eq!(instance.pg_version, "16");

        let listed = core.list_postgres_instances().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, instance.id);

        let fetched = core.get_postgres_instance(instance.id).unwrap();
        assert_eq!(fetched.label, "main");

        let envelope = events.try_recv().expect("event was broadcast");
        match envelope.event {
            Event::PostgresInstanceCreated { instance_id, .. } => assert_eq!(instance_id, instance.id),
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn get_postgres_instance_rejects_unknown_id() {
        let core = Core::open(":memory:").expect("open in-memory db");
        let err = core.get_postgres_instance(Uuid::new_v4()).unwrap_err();
        assert!(matches!(err, CoreError::NotFound(_)));
    }

    #[test]
    fn postgres_instance_state_is_stopped_before_any_start() {
        let core = Core::open(":memory:").expect("open in-memory db");
        let instance = core.create_postgres_instance("main", "16", 55421, "/tmp/pg-never-started").unwrap();
        assert_eq!(core.postgres_instance_state(instance.id).unwrap(), PgState::Stopped);
    }

    #[test]
    fn postgres_instance_state_rejects_unknown_id() {
        let core = Core::open(":memory:").expect("open in-memory db");
        let err = core.postgres_instance_state(Uuid::new_v4()).unwrap_err();
        assert!(matches!(err, CoreError::NotFound(_)));
    }

    /// The concrete proof of the "let's not limit to one Postgres" decision:
    /// two distinct, independently-configured instances, started and stopped
    /// independently through `Core`, with the right events landing for each
    /// — not a singleton, not sharing a supervisor, not interfering with each
    /// other's state.
    #[tokio::test]
    async fn two_distinct_postgres_instances_run_independently() {
        let dir_a = tempfile::TempDir::new().unwrap();
        let dir_b = tempfile::TempDir::new().unwrap();
        if postgres::discover_system_bin_dir().is_none() {
            eprintln!("skipping: no system postgres installed in this environment");
            return;
        }

        let core = Core::open(":memory:").expect("open in-memory db");
        let a = core.create_postgres_instance("a", "16", 55430, dir_a.path().to_str().unwrap()).unwrap();
        let b = core.create_postgres_instance("b", "15", 55431, dir_b.path().to_str().unwrap()).unwrap();

        core.start_postgres_instance(a.id).await.expect("instance a should start");
        assert!(matches!(core.postgres_instance_state(a.id).unwrap(), PgState::Running { .. }));
        assert_eq!(core.postgres_instance_state(b.id).unwrap(), PgState::Stopped, "starting a must not start b");

        core.start_postgres_instance(b.id).await.expect("instance b should start independently");
        assert!(matches!(core.postgres_instance_state(a.id).unwrap(), PgState::Running { .. }), "a should still be running");
        assert!(matches!(core.postgres_instance_state(b.id).unwrap(), PgState::Running { .. }));

        core.stop_postgres_instance(a.id).await.expect("instance a should stop");
        assert_eq!(core.postgres_instance_state(a.id).unwrap(), PgState::Stopped);
        assert!(matches!(core.postgres_instance_state(b.id).unwrap(), PgState::Running { .. }), "stopping a must not stop b");

        core.stop_postgres_instance(b.id).await.expect("instance b should stop");
        assert_eq!(core.postgres_instance_state(b.id).unwrap(), PgState::Stopped);

        let events = core.recent_events(100).unwrap();
        let running_count = events.iter().filter(|e| matches!(e.event, Event::PostgresRunning { .. })).count();
        assert_eq!(running_count, 2, "both instances should have emitted their own PostgresRunning event");
    }

    // --- Odoo server supervision (task 2.2 completion) ----------------------
    //
    // Real `odoo-bin` only exists once a real Odoo checkout is present on
    // this machine — this sandbox's own egress policy blocks
    // github.com/codeload.github.com, the only place it's distributed from
    // (see `odoo.rs`'s and `CONTINUE_LOCALLY.md`'s notes on this). So, same
    // pattern as `core_with_running_postgres_and_server`'s own Postgres
    // check, this test skips itself (not a silent pass — an explicit
    // `eprintln!`) unless a real checkout is pointed at via env var, rather
    // than mocking `odoo-bin` and only proving something Odoo-agnostic
    // (`odoo.rs`'s own tests already do that with a stand-in HTTP server).

    fn odoo_checkout_root_for_tests() -> Option<std::path::PathBuf> {
        let root = std::env::var_os("ORCHESTRATOR_TEST_ODOO_CHECKOUT").map(std::path::PathBuf::from)?;
        if root.join("odoo-bin").is_file() {
            Some(root)
        } else {
            eprintln!("ORCHESTRATOR_TEST_ODOO_CHECKOUT={} has no odoo-bin — skipping", root.display());
            None
        }
    }

    fn discover_uv_bin_for_tests() -> std::path::PathBuf {
        for dir in std::env::split_paths(&std::env::var("PATH").unwrap_or_default()) {
            let candidate = dir.join("uv");
            if candidate.is_file() {
                return candidate;
            }
        }
        let home_local = std::env::var("HOME").ok().map(|h| std::path::PathBuf::from(h).join(".local/bin/uv"));
        if let Some(p) = home_local {
            if p.is_file() {
                return p;
            }
        }
        panic!("this test needs `uv` installed to provision a real venv for real odoo-bin");
    }

    /// The end-to-end proof task 2.2 was left unfinished for: a real
    /// `odoo-bin`, from a real checkout, run under a real (freshly
    /// provisioned, unless `ORCHESTRATOR_TEST_ODOO_VENV_PYTHON` names an
    /// already-provisioned one) Python venv, against this server's real
    /// registered addons source and its real running `PostgresInstance` —
    /// actually serving HTTP, not a stand-in. Also proves `OdooServer::state`
    /// (unlike `PostgresInstance`'s) is persisted through `Core`'s normal
    /// `get_server`, not just held in-memory.
    #[tokio::test]
    async fn start_server_runs_a_real_odoo_bin_and_serves_http() {
        let Some((core, server, dir)) = core_with_running_postgres_and_server().await else { return };
        let Some(checkout_root) = odoo_checkout_root_for_tests() else {
            eprintln!("skipping: set ORCHESTRATOR_TEST_ODOO_CHECKOUT to a real Odoo checkout to run this test");
            return;
        };

        core.create_addons_source(server.id, "odoo-core", checkout_root.join("addons").to_string_lossy(), SourceKind::Core, 0).unwrap();

        let venv_python = if let Some(existing) = std::env::var_os("ORCHESTRATOR_TEST_ODOO_VENV_PYTHON").map(std::path::PathBuf::from) {
            existing
        } else {
            let runtime_config =
                python_runtime::PythonRuntimeConfig { uv_bin: discover_uv_bin_for_tests(), install_dir: dir.path().join("python") };
            python_runtime::ensure_python_installed(&runtime_config, "3.11").await.expect("uv should provision a real python 3.11");
            let venv_python = python_runtime::ensure_venv(&runtime_config, "3.11", &dir.path().join("venv"))
                .await
                .expect("uv venv should succeed");
            let requirements_txt = std::fs::read_to_string(checkout_root.join("requirements.txt"))
                .expect("a real Odoo checkout has a requirements.txt");
            // Odoo's own requirements.txt trails every environment-marker
            // line with a `# comment` explaining the pin — strip it before
            // it reaches `uv`, which parses `;`-markers but not a trailing
            // `#` on the same line (that's requirements.txt-file syntax,
            // not something a bare CLI specifier string supports).
            let requirements: Vec<String> = requirements_txt
                .lines()
                .map(|l| l.split('#').next().unwrap_or("").trim())
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect();
            python_runtime::install_requirements(&runtime_config, &venv_python, &requirements)
                .await
                .expect("odoo's own real requirements should install into the venv");
            // A second real landmine, distinct from `install_requirements`'s
            // own psycopg2/python-ldap substitutions: `odoo/modules/module.py`
            // imports `pkg_resources` directly, which recent `setuptools`
            // releases (81+) no longer vendor. `requirements.txt` doesn't
            // pin `setuptools` itself, so without this the venv resolves the
            // latest one and real `odoo-bin` fails at import time.
            python_runtime::install_requirements(&runtime_config, &venv_python, &["setuptools<81".to_string()])
                .await
                .expect("pinning an older setuptools (for pkg_resources) should succeed");
            venv_python
        };

        let runtime_dir = dir.path().join("servers");
        core.start_server(server.id, checkout_root.clone(), venv_python, runtime_dir)
            .await
            .expect("real odoo-bin should start and become ready");

        let fetched = core.get_server(server.id).unwrap();
        assert_eq!(fetched.state, ServerState::Running, "state must be persisted through Core::get_server, not just live in-memory");
        assert!(fetched.started_at.is_some(), "started_at must be set once the server is actually Running, for uptime display");

        let events = core.recent_events(100).unwrap();
        assert!(events.iter().any(|e| matches!(e.event, Event::ServerStarted { server_id } if server_id == server.id)));

        core.stop_server(server.id).await.expect("real odoo-bin should stop cleanly");
        let fetched = core.get_server(server.id).unwrap();
        assert_eq!(fetched.state, ServerState::Stopped);
        assert!(fetched.started_at.is_none(), "started_at must be cleared once the server stops, so it can't report stale uptime");

        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }

    /// Real `odoo-bin` output must be kept, not thrown away. Before this,
    /// the supervisor streamed every line and `Core` dropped all of them —
    /// so the app could tell you an Odoo had crashed but never why.
    #[tokio::test]
    async fn a_running_odoos_real_output_is_kept_and_streamed() {
        let Some((core, server, _dir)) = core_with_running_postgres_and_server().await else { return };
        let Some(checkout_root) = odoo_checkout_root_for_tests() else { return };
        let Some(venv_python) = std::env::var_os("ORCHESTRATOR_TEST_ODOO_VENV_PYTHON").map(std::path::PathBuf::from) else {
            eprintln!("skipping: set ORCHESTRATOR_TEST_ODOO_VENV_PYTHON to a provisioned venv");
            return;
        };
        core.create_addons_source(server.id, "odoo-core", checkout_root.join("addons").to_string_lossy(), SourceKind::Core, 0).unwrap();

        assert!(core.recent_logs(server.id, 100).is_empty(), "nothing has run yet, so there is nothing to show");

        let mut live = core.subscribe_logs();
        let dir = tempfile::TempDir::new().unwrap();
        core.start_server(server.id, checkout_root, venv_python, dir.path().join("run")).await.unwrap();

        // Odoo announces its own version on startup; that line is enough to
        // prove real process output reached us.
        let kept = core.recent_logs(server.id, 500);
        assert!(!kept.is_empty(), "a started Odoo must have produced output");
        assert!(
            kept.iter().any(|l| l.line.contains("odoo")),
            "the buffer should hold Odoo's own log lines: {:?}",
            kept.iter().take(5).map(|l| &l.line).collect::<Vec<_>>()
        );
        assert!(kept.iter().all(|l| l.server_id == server.id), "every line must be attributed to the Odoo that wrote it");

        // And a live subscriber sees them as they happen.
        let streamed = tokio::time::timeout(std::time::Duration::from_secs(5), live.recv()).await;
        assert!(streamed.is_ok(), "a subscriber should have received live output");

        core.stop_server(server.id).await.unwrap();
        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }

    /// The buffer is bounded: a chatty Odoo must not grow memory forever.
    #[test]
    fn the_log_buffer_keeps_the_most_recent_lines_and_drops_the_oldest() {
        let core = Core::open(":memory:").unwrap();
        let server_id = Uuid::new_v4();
        for i in 0..(LOG_BUFFER_LINES + 250) {
            core.record_log_line(server_id, "stdout", format!("line {i}"));
        }
        let kept = core.recent_logs(server_id, LOG_BUFFER_LINES * 2);
        assert_eq!(kept.len(), LOG_BUFFER_LINES, "the buffer must stay bounded");
        assert_eq!(kept.first().unwrap().line, format!("line {}", 250), "the oldest lines are the ones dropped");
        assert_eq!(kept.last().unwrap().line, format!("line {}", LOG_BUFFER_LINES + 249), "the newest line is kept");

        // And asking for fewer gives the *latest* ones, which is what a log
        // pane wants — not the first N from the start of time.
        let tail = core.recent_logs(server_id, 10);
        assert_eq!(tail.len(), 10);
        assert_eq!(tail.last().unwrap().line, format!("line {}", LOG_BUFFER_LINES + 249));
    }

    #[tokio::test]
    async fn stop_server_without_ever_starting_is_a_harmless_no_op() {
        let (core, pg_id, _dir) = core_with_postgres_instance();
        let server = core.create_server("acme", "17.0", 8069, pg_id).unwrap();
        core.stop_server(server.id).await.expect("stopping a server that was never started should be Ok, not an error");
    }

    // --- module install/upgrade/uninstall (task 2.4) ------------------------
    //
    // Same real-checkout requirement as `start_server_runs_a_real_odoo_bin_and_serves_http`
    // — see that test's own comment for why this skips itself (not silently)
    // without one. Uses `google_account` as the module under test: real,
    // genuinely installable, but with a minimal dependency chain
    // (`base_setup` → `base`/`web`) so this doesn't pay for a heavy module's
    // own asset bundling on top of the framework bootstrap every install
    // already requires.

    #[tokio::test]
    async fn install_upgrade_and_uninstall_a_real_module_against_a_real_database() {
        let Some((core, server, dir)) = core_with_running_postgres_and_server().await else { return };
        let Some(checkout_root) = odoo_checkout_root_for_tests() else {
            eprintln!("skipping: set ORCHESTRATOR_TEST_ODOO_CHECKOUT to a real Odoo checkout to run this test");
            return;
        };
        let Some(venv_python) = std::env::var_os("ORCHESTRATOR_TEST_ODOO_VENV_PYTHON").map(std::path::PathBuf::from) else {
            eprintln!(
                "skipping: set ORCHESTRATOR_TEST_ODOO_VENV_PYTHON to an already-provisioned venv \
                 (see start_server_runs_a_real_odoo_bin_and_serves_http for how to provision one)"
            );
            return;
        };

        core.create_addons_source(server.id, "odoo-core", checkout_root.join("addons").to_string_lossy(), SourceKind::Core, 0).unwrap();
        let db = core.create_database(server.id, "orch_modtest").await.unwrap();

        let instance = core.get_postgres_instance(server.postgres_instance_id).unwrap();
        let bin_dir = postgres::discover_system_bin_dir().unwrap();
        let module_state = |name: &str| {
            psql_exec(
                &bin_dir,
                std::path::Path::new(&instance.data_dir),
                instance.port,
                &db.name,
                &format!("SELECT state FROM ir_module_module WHERE name = '{name}'"),
            )
        };

        let runtime_dir = dir.path().join("servers");
        let module = "google_account".to_string();

        let states_before = core.database_module_states(db.id).await.unwrap();
        assert!(states_before.is_empty(), "a bare, never-Odoo-initialized database must report no module state, not an error");

        core.install_modules(db.id, &[module.clone()], checkout_root.clone(), venv_python.clone(), runtime_dir.clone())
            .await
            .expect("installing a real module against a bare database should bootstrap the framework and succeed");
        assert_eq!(module_state(&module), "installed", "the module must actually be installed in ir_module_module, not just claimed");
        let events = core.recent_events(200).unwrap();
        assert!(events
            .iter()
            .any(|e| matches!(&e.event, Event::ModuleInstalled { database_id, technical_name } if *database_id == db.id && technical_name == &module)));

        let states_after_install = core.database_module_states(db.id).await.unwrap();
        let installed_row = states_after_install.iter().find(|s| s.technical_name == module).expect("the just-installed module must appear in real module state");
        assert_eq!(installed_row.state, "installed");
        assert!(installed_row.installed_version.is_some(), "an installed module must report a real installed_version, not None");
        assert!(states_after_install.len() > 1, "a real Odoo database has many module rows, not just the one just installed");

        core.upgrade_modules(db.id, &[module.clone()], checkout_root.clone(), venv_python.clone(), runtime_dir.clone())
            .await
            .expect("upgrading an already-installed real module should succeed");
        assert_eq!(module_state(&module), "installed", "an upgrade must leave the module installed");
        let events = core.recent_events(200).unwrap();
        let upgraded_version = events.iter().find_map(|e| match &e.event {
            Event::ModuleUpgraded { database_id, technical_name, to_version } if *database_id == db.id && technical_name == &module => {
                Some(to_version.clone())
            }
            _ => None,
        });
        assert!(
            upgraded_version.is_some_and(|v| v != "unknown" && !v.is_empty()),
            "must report a real version read back from ir_module_module, not a placeholder"
        );

        core.uninstall_modules(db.id, &[module.clone()], checkout_root.clone(), venv_python.clone(), runtime_dir.clone())
            .await
            .expect("uninstalling a real module should succeed");
        assert_eq!(module_state(&module), "uninstalled", "the module must actually be uninstalled in ir_module_module, not just claimed");
        let events = core.recent_events(200).unwrap();
        assert!(events
            .iter()
            .any(|e| matches!(&e.event, Event::ModuleUninstalled { database_id, technical_name } if *database_id == db.id && technical_name == &module)));

        let states_after_uninstall = core.database_module_states(db.id).await.unwrap();
        let uninstalled_row = states_after_uninstall.iter().find(|s| s.technical_name == module).expect("the module must still have a row (just uninstalled, not gone)");
        assert_eq!(uninstalled_row.state, "uninstalled");

        core.stop_postgres_instance(server.postgres_instance_id).await.unwrap();
    }
}
