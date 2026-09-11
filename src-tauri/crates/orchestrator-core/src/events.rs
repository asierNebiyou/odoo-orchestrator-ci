//! The domain event catalog from `odoo-orchestrator-technical-design.md`.
//! Every state change happens by appending one of these, persisting it to the
//! event log, then broadcasting it — the shell UI, the editor sidecar, and any
//! future plugin all subscribe to this instead of polling.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum Event {
    ProjectCreated { project_id: Uuid, name: String },
    ProjectRenamed { project_id: Uuid, name: String },
    ProjectDeleted { project_id: Uuid },
    /// A project's *setup* was copied — its Odoos and their addons paths,
    /// deliberately **not** their databases. `server_count` is how many
    /// Odoo definitions were reproduced, so a listener can say exactly what
    /// the copy did without re-querying.
    ProjectDuplicated { project_id: Uuid, source_project_id: Uuid, server_count: u32 },

    ServerCreated { server_id: Uuid, name: String, odoo_version: String, postgres_instance_id: Uuid },
    ServerStarted { server_id: Uuid },
    ServerStopped { server_id: Uuid },
    ServerCrashed { server_id: Uuid, exit_code: i32 },

    DatabaseCreated { database_id: Uuid, server_id: Uuid, name: String },
    DatabaseDuplicated { database_id: Uuid, source_database_id: Uuid },
    /// Carries the name and server as well as the id, because by the time
    /// anyone reads this the row is gone — and an event log that renders
    /// "dropped database 3f2a1b0c" for the one event where the name
    /// matters most is a log nobody can use. The `server_id` is what makes
    /// restoring it from a backup possible at all.
    DatabaseDropped { database_id: Uuid, server_id: Uuid, name: String },
    /// `path` is where the dump actually landed, and `server_id` is where
    /// it would be restored to. Both are recorded because an event saying
    /// a backup happened, but not where it went or where it belongs, can't
    /// be acted on — and Activity's rule is that a row offers an inverse
    /// only when that inverse can really complete.
    DatabaseBackedUp { database_id: Uuid, server_id: Uuid, format: String, path: String },
    DatabaseRestored { database_id: Uuid },
    /// Odoo's own neutralization ran over this database: mail servers
    /// deactivated and their credentials wiped, crons stopped, payment
    /// provider credentials cleared.
    DatabaseNeutralized { database_id: Uuid },
    /// A pre-initialized database was kept so the next one of the same
    /// shape is a copy rather than a boot.
    TemplateCached { key: String, modules: Vec<String> },

    /// A major-version migration, hop by hop. Recorded separately per hop
    /// because a chain that stops in the middle is the normal interesting
    /// case, and "which one stopped it" is the question afterwards.
    UpgradeStarted { database_id: Uuid, working_database_id: Uuid, from: String, to: String, hops: u32 },
    UpgradeHopFinished { working_database_id: Uuid, from: String, to: String },
    UpgradeHopFailed { working_database_id: Uuid, from: String, to: String, message: String },
    UpgradeFinished { database_id: Uuid, working_database_id: Uuid, to: String },

    ModulesScanned { server_id: Uuid, module_count: u32, shadow_count: u32 },
    ModuleInstalled { database_id: Uuid, technical_name: String },
    ModuleUpgraded { database_id: Uuid, technical_name: String, to_version: String },
    ModuleUninstalled { database_id: Uuid, technical_name: String },

    PostgresInstanceCreated { instance_id: Uuid, label: String, pg_version: String },
    PostgresStarting { instance_id: Uuid },
    PostgresRunning { instance_id: Uuid, pid: u32 },
    PostgresStopping { instance_id: Uuid },
    PostgresStopped { instance_id: Uuid },
    PostgresCrashed { instance_id: Uuid, exit_code: Option<i32> },

    SnapshotCreated { snapshot_id: Uuid, database_id: Uuid, name: String, has_filestore: bool },
    SnapshotDeleted { snapshot_id: Uuid },
    /// A database was reverted **in place** to an existing snapshot (task
    /// 3.3) — distinct from `SnapshotCreated`/`SnapshotDeleted`, which never
    /// touch a server's live database. `counter_snapshot_id` is set when the
    /// caller asked for a safety snapshot of the live state to be taken
    /// before the destructive part of the revert (the friction ladder's
    /// "counter-snapshot offered" element for this rung), so a listener can
    /// show exactly what the revert itself can be undone with.
    DatabaseRevertedToSnapshot { database_id: Uuid, snapshot_id: Uuid, counter_snapshot_id: Option<Uuid> },

    EditorAttached { database_id: Uuid },
}

impl Event {
    /// The `type` discriminant, e.g. "server_started" — used as the SQLite
    /// column so the event log stays queryable without deserializing every row.
    pub fn kind(&self) -> &'static str {
        match self {
            Event::ProjectCreated { .. } => "project_created",
            Event::ProjectRenamed { .. } => "project_renamed",
            Event::ProjectDeleted { .. } => "project_deleted",
            Event::ProjectDuplicated { .. } => "project_duplicated",
            Event::ServerCreated { .. } => "server_created",
            Event::ServerStarted { .. } => "server_started",
            Event::ServerStopped { .. } => "server_stopped",
            Event::ServerCrashed { .. } => "server_crashed",
            Event::DatabaseCreated { .. } => "database_created",
            Event::DatabaseDuplicated { .. } => "database_duplicated",
            Event::DatabaseDropped { .. } => "database_dropped",
            Event::DatabaseBackedUp { .. } => "database_backed_up",
            Event::DatabaseRestored { .. } => "database_restored",
            Event::DatabaseNeutralized { .. } => "database_neutralized",
            Event::TemplateCached { .. } => "template_cached",
            Event::UpgradeStarted { .. } => "upgrade_started",
            Event::UpgradeHopFinished { .. } => "upgrade_hop_finished",
            Event::UpgradeHopFailed { .. } => "upgrade_hop_failed",
            Event::UpgradeFinished { .. } => "upgrade_finished",
            Event::ModulesScanned { .. } => "modules_scanned",
            Event::ModuleInstalled { .. } => "module_installed",
            Event::ModuleUpgraded { .. } => "module_upgraded",
            Event::ModuleUninstalled { .. } => "module_uninstalled",
            Event::PostgresInstanceCreated { .. } => "postgres_instance_created",
            Event::PostgresStarting { .. } => "postgres_starting",
            Event::PostgresRunning { .. } => "postgres_running",
            Event::PostgresStopping { .. } => "postgres_stopping",
            Event::PostgresStopped { .. } => "postgres_stopped",
            Event::PostgresCrashed { .. } => "postgres_crashed",
            Event::SnapshotCreated { .. } => "snapshot_created",
            Event::SnapshotDeleted { .. } => "snapshot_deleted",
            Event::DatabaseRevertedToSnapshot { .. } => "database_reverted_to_snapshot",
            Event::EditorAttached { .. } => "editor_attached",
        }
    }
}

/// The envelope actually persisted to the event log and broadcast over the
/// local API's WS stream — an event plus when it happened and a stable id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub id: Uuid,
    pub occurred_at: DateTime<Utc>,
    /// How long the operation took, when it's something that takes real
    /// time (creating a database, restoring a snapshot, updating modules).
    /// `None` for instant state changes.
    ///
    /// Carried on the envelope rather than added to two dozen event
    /// variants: it's a property of *recording* the event, not of what
    /// happened. It's what lets the Stats screen say "restoring takes
    /// about 8s" from measured facts instead of an estimate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(flatten)]
    pub event: Event,
}

impl EventEnvelope {
    pub fn new(event: Event) -> Self {
        Self { id: Uuid::new_v4(), occurred_at: Utc::now(), duration_ms: None, event }
    }

    /// As `new`, but recording how long the operation took.
    pub fn timed(event: Event, took: std::time::Duration) -> Self {
        Self {
            id: Uuid::new_v4(),
            occurred_at: Utc::now(),
            duration_ms: Some(took.as_millis() as u64),
            event,
        }
    }
}
