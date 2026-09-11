//! Domain model. Deliberately plain data — no process-handling logic lives here.
//! See `odoo-orchestrator-technical-design.md`: the core owns state, sidecars do the running.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Where a module's code actually comes from — the provenance model the whole
/// "shadowing detector" feature (task 1.3 in the breakdown) is built around.
/// Odoo itself has no concept of this; it only exists in addons-path order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Private,
    Oca,
    Core,
}

/// One entry on a server's addons_path, in load order. Rank 0 loads first and
/// wins any technical-name collision (the "SHADOWED" case in Modules.dc.html).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddonsSource {
    pub id: Uuid,
    pub server_id: Uuid,
    pub label: String,
    pub path_or_url: String,
    pub kind: SourceKind,
    pub rank: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum ServerState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Crashed { exit_code: i32 },
}

impl Default for ServerState {
    fn default() -> Self {
        ServerState::Stopped
    }
}

/// A named grouping of Odoos — the top level of the UI's navigation, and
/// the unit a user actually thinks in ("the Acme job", "my OCA sandbox").
///
/// This is deliberately **only** an organizational container, not a second
/// topology rule: it owns no ports, no Postgres instance and no addons
/// path, and moving an Odoo between projects changes nothing about how it
/// runs. The real constraint that forces separation is still
/// `addons_path`-per-process (see `odoo-orchestrator-runtime-architecture.md`),
/// which lives on `OdooServer`. A project is what makes a machine holding
/// four clients' work navigable; it is not what makes their code isolated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    /// Index into the UI's fixed six-swatch accent palette. Carried here
    /// rather than derived from the name's hash so renaming a project
    /// doesn't silently re-colour it out from under someone who has learned
    /// to find it by colour.
    pub color: u8,
    pub created_at: DateTime<Utc>,
}

impl Project {
    pub fn new(name: impl Into<String>, color: u8) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            color: color % 6,
            created_at: Utc::now(),
        }
    }
}

/// What this Odoo should do when code on disk changes.
///
/// Deliberately two separate switches, because they are two genuinely
/// different mechanisms with different costs:
///
/// * `reload_python` hands the job to **Odoo's own** `--dev=reload`, which
///   watches `.py` files, compiles each change before acting (so a syntax
///   error logs instead of restarting into a broken state) and restarts
///   the process itself. Reimplementing that here would be strictly worse.
/// * `update_on_data_change` is the piece Odoo has **no** answer for:
///   changes to data XML and CSV need `-u <module>` to take effect, and
///   nothing in Odoo watches for them. That one is this app's own watcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ReloadPolicy {
    /// Adds `dev_mode = reload,qweb,xml` to the generated `odoo.conf`.
    pub reload_python: bool,
    /// Runs `-u <module>` when that module's data files change.
    pub update_on_data_change: bool,
}

/// A running (or not-yet-running) Odoo process. Because addons_path is parsed
/// once at server startup, everything version- and module-availability-related
/// is scoped to the server, not the database — that's the load-bearing
/// constraint behind the whole topology feature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OdooServer {
    pub id: Uuid,
    /// Which `Project` this Odoo is filed under. `None` only for rows that
    /// predate projects existing; `Core::open` adopts any such row into a
    /// default project on startup, so a freshly-opened Core never hands one
    /// out with `None` in practice.
    pub project_id: Option<Uuid>,
    pub name: String,
    pub odoo_version: String,
    pub port: u16,
    /// Which `PostgresInstance` this server's databases live on. Required,
    /// not optional — every server needs *some* Postgres backend — but
    /// deliberately not defaulted to "the only instance" inside `OdooServer`
    /// itself: the caller (eventually a UI flow) decides, per the topology
    /// rule in `odoo-orchestrator-runtime-architecture.md` (share by
    /// default when nothing forces separation, a dedicated instance only
    /// when something genuinely does).
    pub postgres_instance_id: Uuid,
    pub state: ServerState,
    /// What to do when code on disk changes. Off by default: restarting
    /// somebody's Odoo without being asked is exactly the kind of surprise
    /// this product's design rules exist to prevent.
    pub reload: ReloadPolicy,
    /// When the currently-running `odoo-bin` process last transitioned to
    /// `Running` — `None` whenever `state` isn't `Running` (cleared on every
    /// `Stopped`/`Crashed` transition, set on every `Running` one). This is
    /// the one source of truth for "uptime" in the UI; nothing here assumes
    /// the process has been up continuously since `created_at`.
    pub started_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Where a database's contents came from. Recorded because "is this data
/// safe to poke at" is answered by provenance, not by anything inside the
/// database: one you created is empty and harmless, one restored from a
/// client's dump may hold live mail servers, real payment credentials and
/// real people's addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseOrigin {
    /// Made here, empty to begin with.
    Created,
    /// A copy of another database on this machine.
    Duplicated,
    /// Restored from a dump — the one that may carry production data in
    /// from somewhere else.
    Restored,
    /// Already existed in the cluster and was adopted; nothing here knows
    /// what made it or what's in it.
    Adopted,
}

impl DatabaseOrigin {
    /// Whether the contents came from outside this machine's own making,
    /// and so might be somebody's real data.
    pub fn may_hold_foreign_data(self) -> bool {
        matches!(self, Self::Restored | Self::Adopted)
    }
}

/// A single Odoo database on a server. Module *install state* is per-database
/// even though module *availability* is per-server — see task 2.4's note.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Database {
    pub id: Uuid,
    pub server_id: Uuid,
    pub name: String,
    /// How this database came to exist. See `DatabaseOrigin`.
    pub origin: DatabaseOrigin,
    /// The hostname this database answers on. `None` means the default,
    /// `<name>.localhost`.
    ///
    /// **Not `<name>.odoo`, deliberately.** `.odoo` is not a reserved
    /// TLD, and ICANN's new-gTLD application window opened on 30 April
    /// 2026 — including `.brand` TLDs, which is exactly what a company
    /// called Odoo would apply for. This is precisely how `.dev` broke
    /// every local development setup using it: Google took the TLD and
    /// put it on the HSTS preload list, and overnight every `myapp.dev`
    /// was force-redirected to HTTPS. `.localhost` is reserved by
    /// RFC 6761 and resolves to loopback in modern browsers with no
    /// setup at all. `.test` is the other reserved option and is the one
    /// to use if a resolver entry is ever installed, since it can never
    /// be delegated either.
    ///
    /// Odoo's own `dbfilter = ^%d$` routes by the *first label* of the
    /// Host header, so a database called `acme` is natively reachable at
    /// `acme.<anything>` — the suffix is irrelevant to Odoo, which is
    /// what makes changing it a one-line decision here. That's also why
    /// a custom domain is resolved by the proxy rewriting Host to
    /// `<name>.localhost` before forwarding rather than by reconfiguring
    /// Odoo.
    pub domain: Option<String>,
    pub size_bytes: u64,
    pub last_backup_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl OdooServer {
    pub fn new(name: impl Into<String>, odoo_version: impl Into<String>, port: u16, postgres_instance_id: Uuid) -> Self {
        Self {
            id: Uuid::new_v4(),
            project_id: None,
            name: name.into(),
            odoo_version: odoo_version.into(),
            port,
            postgres_instance_id,
            state: ServerState::Stopped,
            reload: ReloadPolicy::default(),
            started_at: None,
            created_at: Utc::now(),
        }
    }

    /// File this Odoo under a project. Separate from `new` because a
    /// project is an organizational choice made by whoever is creating the
    /// Odoo, not a property the domain object can default for itself.
    pub fn in_project(mut self, project_id: Uuid) -> Self {
        self.project_id = Some(project_id);
        self
    }
}

impl Database {
    pub fn new(server_id: Uuid, name: impl Into<String>) -> Self {
        Self::from_origin(server_id, name, DatabaseOrigin::Created)
    }

    pub fn from_origin(server_id: Uuid, name: impl Into<String>, origin: DatabaseOrigin) -> Self {
        Self {
            id: Uuid::new_v4(),
            server_id,
            name: name.into(),
            origin,
            domain: None,
            size_bytes: 0,
            last_backup_at: None,
            created_at: Utc::now(),
        }
    }
}

/// A supervised Postgres cluster (task 2.1). Deliberately **not** a
/// singleton and **not** owned by a single `OdooServer` — per the "we might
/// genuinely need multiple Postgreses" decision (different major versions
/// under test, or a user who just wants project isolation), an app can have
/// any number of these, and an `OdooServer` will reference *which* one it
/// uses (once 2.2/2.3 wire that up) rather than the app assuming exactly
/// one. This mirrors the topology rule `odoo-orchestrator-runtime-architecture.md`
/// already established for Odoo servers themselves: share by default when
/// nothing forces separation, but nothing stops a separate instance when it
/// genuinely is needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostgresInstance {
    pub id: Uuid,
    pub label: String,
    /// Informational — which Postgres major version this instance is
    /// expected to be (e.g. "16"). Not yet used to select a bundled binary
    /// (see `postgres::discover_system_bin_dir`'s doc comment); once
    /// per-version bundled runtimes exist, this is what picks one.
    pub pg_version: String,
    pub port: u16,
    pub data_dir: String,
    pub created_at: DateTime<Utc>,
}

impl Database {
    /// What this database actually answers on — its own domain, or the
    /// default derived from its name.
    pub fn hostname(&self) -> String {
        self.domain.clone().unwrap_or_else(|| format!("{}.localhost", self.name))
    }
}

impl PostgresInstance {
    pub fn new(label: impl Into<String>, pg_version: impl Into<String>, port: u16, data_dir: impl Into<String>) -> Self {
        Self { id: Uuid::new_v4(), label: label.into(), pg_version: pg_version.into(), port, data_dir: data_dir.into(), created_at: Utc::now() }
    }
}

/// A point-in-time, instantly-created copy of one database (and, when a
/// filestore path is provided, one directory tree) — the primitive task 3.1
/// exists to build, ahead of git-branch binding (3.2) and one-click revert
/// (3.3). Deliberately keyed to an arbitrary `name`, not necessarily a git
/// branch — nothing here assumes git is involved at all.
///
/// Carries `server_id` directly (not just `database_id`) so a snapshot
/// stays usable — droppable, at minimum — even after its source `Database`
/// is itself dropped via `Core::drop_database`. That's the entire point of
/// a snapshot: it must outlive the thing it was taken from. `database_id`
/// and `source_database_name` are kept purely as provenance for display —
/// `database_id` can become a dangling reference if the source is later
/// dropped, which the caller should treat as "source no longer exists,"
/// not as this snapshot being invalid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: Uuid,
    pub server_id: Uuid,
    pub database_id: Uuid,
    /// The source database's name at the moment this snapshot was taken —
    /// display-only provenance, kept even if that database is later
    /// renamed or dropped.
    pub source_database_name: String,
    /// The real Postgres database name backing this snapshot — a
    /// `CREATE DATABASE ... TEMPLATE` duplicate of the source database at
    /// snapshot time (see `pg_admin::duplicate_database`). Deliberately
    /// NOT tracked in the `databases` table/list: a snapshot is a frozen
    /// artifact, not a live database the user works in day to day.
    pub snapshot_database_name: String,
    /// The user-facing label — a git branch name is the expected common
    /// case once 3.2 exists, but this module doesn't require or assume
    /// that; any string is valid.
    pub name: String,
    /// The git branch the linked repository was on when this was taken,
    /// when there was one to read.
    ///
    /// Recorded rather than inferred from `name`: people rename snapshots,
    /// and a label that merely *looks* like a branch is not evidence of
    /// anything. This is what lets the app say "you're on `feature/x` and
    /// this snapshot is from `feature/x`" instead of guessing from a
    /// string. `None` means there was no linked repo, or it wasn't on a
    /// resolvable branch — an ordinary state, not a failure.
    pub git_branch: Option<String>,
    /// Set only when a filestore directory was actually snapshotted
    /// alongside the database — see `filestore::snapshot_directory`. `None`
    /// is the honest, common case today: there's no real Odoo filestore in
    /// this project yet (see `odoo.rs`'s module doc comment for why), so a
    /// snapshot taken now is Postgres-only.
    pub filestore_snapshot_path: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl Snapshot {
    pub fn new(
        server_id: Uuid,
        database_id: Uuid,
        source_database_name: impl Into<String>,
        snapshot_database_name: impl Into<String>,
        name: impl Into<String>,
        filestore_snapshot_path: Option<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            server_id,
            database_id,
            source_database_name: source_database_name.into(),
            snapshot_database_name: snapshot_database_name.into(),
            name: name.into(),
            filestore_snapshot_path,
            git_branch: None,
            created_at: Utc::now(),
        }
    }

    /// Records the branch this was taken on.
    pub fn on_branch(mut self, branch: Option<String>) -> Self {
        self.git_branch = branch;
        self
    }
}
