//! REST-ish routes over `orchestrator-core`. Handlers call the (sync, SQLite-
//! backed) core methods directly — fine for the cheap reads/writes here, but
//! anything that spawns or waits on a process (workstreams 2.1/2.2) should
//! use `tokio::task::spawn_blocking` or a proper async supervisor instead of
//! copying this pattern for slow work.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::get;
use axum::Router;
use orchestrator_core::{ReloadPolicy, SourceKind};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/projects", get(list_projects).post(create_project))
        .route("/projects/:project_id", axum::routing::patch(update_project).delete(delete_project))
        .route("/projects/:project_id/duplicate", axum::routing::post(duplicate_project))
        .route("/odoo-runtimes/:version", get(find_odoo_runtime))
        .route("/servers", get(list_servers).post(create_server))
        .route("/servers/:server_id/start", axum::routing::post(start_server))
        .route("/servers/:server_id/stop", axum::routing::post(stop_server))
        .route("/servers/:server_id/databases", get(list_databases_for_server).post(create_database))
        .route("/databases", get(list_all_databases))
        // Deliberately not "/databases/reconcile": that would sit in the
        // same position as "/databases/:database_id", and a router that
        // resolves an id-shaped path by luck is a bug waiting for someone
        // to name a database "reconcile".
        .route("/reconciliation", get(reconcile_databases))
        .route("/maintenance/refresh-sizes", axum::routing::post(refresh_database_sizes))
        .route("/servers/:server_id/databases/adopt", axum::routing::post(adopt_database))
        .route("/databases/:database_id/forget", axum::routing::post(forget_database))
        .route("/disk-usage", get(disk_usage))
        .route("/process-usage", get(process_usage))
        .route("/odoo-copies", get(list_odoo_copies))
        .route("/odoo-copies/:folder", axum::routing::delete(delete_odoo_copy))
        .route("/routes", get(list_routes))
        .route("/databases/:database_id/domain", axum::routing::put(set_database_domain))
        .route("/databases/:database_id/duplicate", axum::routing::post(duplicate_database))
        .route("/databases/:database_id", axum::routing::delete(drop_database))
        .route("/databases/:database_id/backup", axum::routing::post(backup_database))
        .route("/servers/:server_id/databases/restore", axum::routing::post(restore_database))
        .route("/backups", get(list_backups))
        .route("/databases/:database_id/neutralize", axum::routing::post(neutralize_database))
        .route("/neutralization", get(neutralization_states))
        .route("/templates", get(list_templates).delete(clear_templates))
        .route("/databases/:database_id/upgrade-plan", get(upgrade_plan))
        .route("/databases/:database_id/upgrade", axum::routing::post(run_upgrade))
        .route("/upgrade-targets", get(upgrade_targets))
        .route("/addons-discovery", get(discover_addons))
        .route("/servers/:server_id/addons-sources/adopt", axum::routing::post(adopt_addons_roots))
        .route("/databases/:database_id/freshness", get(module_freshness))
        .route("/databases/:database_id/modules/upgrade-batched", axum::routing::post(upgrade_modules_batched))
        .route("/databases/:database_id/snapshots", get(list_snapshots_for_database).post(create_snapshot))
        .route("/snapshots", get(list_all_snapshots))
        .route("/snapshots/:snapshot_id", axum::routing::delete(delete_snapshot))
        .route("/databases/:database_id/revert", axum::routing::post(revert_database_to_snapshot))
        .route("/servers/:server_id/git-branch", get(current_git_branch))
        .route("/servers/:server_id/branch-snapshots", get(snapshots_on_branch))
        .route("/servers/:server_id/reload-policy", axum::routing::patch(set_reload_policy))
        .route("/servers/:server_id/odoo-conf", get(preview_odoo_conf))
        .route("/servers/:server_id/logs", get(server_logs))
        .route("/events", get(list_events))
        .route("/servers/:server_id/addons-sources", get(list_addons_sources).post(create_addons_source))
        .route("/servers/:server_id/modules", get(scan_modules))
        .route("/databases/:database_id/modules/install", axum::routing::post(install_modules))
        .route("/databases/:database_id/modules/upgrade", axum::routing::post(upgrade_modules))
        .route("/databases/:database_id/modules/uninstall", axum::routing::post(uninstall_modules))
        .route("/databases/:database_id/module-states", get(database_module_states))
        .route("/postgres-instances", get(list_postgres_instances).post(create_postgres_instance))
        .route("/postgres-instances/:instance_id", get(get_postgres_instance))
        .route("/postgres-instances/:instance_id/start", axum::routing::post(start_postgres_instance))
        .route("/postgres-instances/:instance_id/stop", axum::routing::post(stop_postgres_instance))
}

async fn health() -> &'static str {
    "ok"
}

fn core_error_to_status(err: orchestrator_core::CoreError) -> StatusCode {
    tracing::error!("core error: {err}");
    match err {
        orchestrator_core::CoreError::NotFound(_) => StatusCode::NOT_FOUND,
        orchestrator_core::CoreError::Postgres(orchestrator_core::PgError::AlreadyRunning) => StatusCode::CONFLICT,
        orchestrator_core::CoreError::Postgres(orchestrator_core::PgError::NotRunning) => StatusCode::CONFLICT,
        orchestrator_core::CoreError::PostgresInstanceNotRunning { .. } => StatusCode::CONFLICT,
        orchestrator_core::CoreError::SnapshotNotForDatabase { .. } => StatusCode::UNPROCESSABLE_ENTITY,
        orchestrator_core::CoreError::SnapshotHasNoFilestore { .. } => StatusCode::UNPROCESSABLE_ENTITY,
        orchestrator_core::CoreError::Odoo(orchestrator_core::OdooError::AlreadyRunning) => StatusCode::CONFLICT,
        // 409, not 500: the request is well-formed and would succeed the
        // moment whatever holds the port lets go. The UI should say "that
        // port is taken", not "something went wrong".
        orchestrator_core::CoreError::Odoo(orchestrator_core::OdooError::PortInUse { .. }) => StatusCode::CONFLICT,
        // 409, not 422: the request is well-formed and would be valid the
        // moment the project is emptied — the UI turns this into "this
        // project still holds N Odoos", not "bad request".
        orchestrator_core::CoreError::ProjectNotEmpty { .. } => StatusCode::CONFLICT,
        // Same reasoning: the request is fine, the name is simply taken.
        orchestrator_core::CoreError::DomainTaken { .. } => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

// --- projects ------------------------------------------------------------

async fn list_projects(State(state): State<AppState>) -> impl IntoResponse {
    match state.core.list_projects() {
        Ok(projects) => Json(projects).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

#[derive(Deserialize)]
struct CreateProjectRequest {
    name: String,
    #[serde(default)]
    color: u8,
}

async fn create_project(State(state): State<AppState>, Json(req): Json<CreateProjectRequest>) -> impl IntoResponse {
    match state.core.create_project(req.name, req.color) {
        Ok(project) => (StatusCode::CREATED, Json(project)).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

#[derive(Deserialize)]
struct UpdateProjectRequest {
    name: String,
    #[serde(default)]
    color: u8,
}

async fn update_project(
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
    Json(req): Json<UpdateProjectRequest>,
) -> impl IntoResponse {
    match state.core.rename_project(project_id, req.name, req.color) {
        Ok(project) => Json(project).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

async fn delete_project(State(state): State<AppState>, Path(project_id): Path<Uuid>) -> impl IntoResponse {
    match state.core.delete_project(project_id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

#[derive(Deserialize)]
struct DuplicateProjectRequest {
    new_name: String,
}

async fn duplicate_project(
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
    Json(req): Json<DuplicateProjectRequest>,
) -> impl IntoResponse {
    match state.core.duplicate_project(project_id, req.new_name) {
        Ok(project) => (StatusCode::CREATED, Json(project)).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

// --- hostname routing ----------------------------------------------------

/// Every hostname the proxy will answer on, and where each one goes.
async fn list_routes(State(state): State<AppState>) -> impl IntoResponse {
    match state.core.routing_table() {
        Ok(routes) => Json(routes).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

#[derive(Deserialize)]
struct SetDomainRequest {
    /// `null` puts the database back on its default `<name>.odoo`.
    domain: Option<String>,
}

async fn set_database_domain(
    State(state): State<AppState>,
    Path(database_id): Path<Uuid>,
    Json(req): Json<SetDomainRequest>,
) -> impl IntoResponse {
    match state.core.set_database_domain(database_id, req.domain.as_deref()) {
        Ok(database) => Json(database).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

// --- odoo runtimes -------------------------------------------------------

#[derive(Serialize)]
struct FoundRuntime {
    found: bool,
    path: Option<String>,
    source: Option<String>,
}

/// Purely a lookup — never downloads. The UI calls this to say whether a
/// version is already here before someone commits to a first start.
async fn find_odoo_runtime(State(state): State<AppState>, Path(version): Path<String>) -> impl IntoResponse {
    match state.core.find_odoo(&version) {
        Ok(Some(runtime)) => Json(FoundRuntime {
            found: true,
            path: Some(runtime.checkout_root.display().to_string()),
            source: Some(format!("{:?}", runtime.source).to_lowercase()),
        })
        .into_response(),
        Ok(None) => Json(FoundRuntime { found: false, path: None, source: None }).into_response(),
        // Not configured yet is "we don't know", not an error page.
        Err(_) => Json(FoundRuntime { found: false, path: None, source: None }).into_response(),
    }
}

// --- servers -------------------------------------------------------------

async fn list_servers(State(state): State<AppState>) -> impl IntoResponse {
    match state.core.list_servers() {
        Ok(servers) => Json(servers).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

#[derive(Deserialize)]
struct CreateServerRequest {
    name: String,
    odoo_version: String,
    port: u16,
    /// Optional: when absent the core uses (or creates) the app's own
    /// database storage. Nobody should have to pick where a database is
    /// kept in order to make one.
    #[serde(default)]
    postgres_instance_id: Option<Uuid>,
    /// Optional so callers predating projects (the existing e2e scripts)
    /// keep working unchanged; the UI always sends one.
    #[serde(default)]
    project_id: Option<Uuid>,
}

async fn create_server(
    State(state): State<AppState>,
    Json(req): Json<CreateServerRequest>,
) -> impl IntoResponse {
    let storage = match req.postgres_instance_id {
        Some(id) => id,
        None => match state.core.ensure_storage().await {
            Ok(instance) => instance.id,
            Err(err) => return core_error_to_status(err).into_response(),
        },
    };
    match state.core.create_server_in_project(req.name, req.odoo_version, req.port, storage, req.project_id) {
        Ok(server) => (StatusCode::CREATED, Json(server)).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

// Starting/stopping a real `odoo-bin` (task 2.2). `checkout_root` and
// `venv_python` are absolute paths on this machine — same posture as
// `create_snapshot`'s `filestore_source` and `restore_database`'s
// `dump_file`: this is a local-desktop app, not a hosted multi-tenant
// service, so a plain filesystem path from the caller is the right shape
// here, not an upload. `venv_python` must already be a provisioned
// interpreter with the checkout's `requirements.txt` installed — this app
// doesn't provision it inline (see `Core::start_server`'s doc comment).

/// Every field is optional now. With none of them, the core finds (or
/// fetches) the right Odoo itself — see `Core::start_server_here`. They
/// remain accepted so a caller with its own checkout can still say so.
#[derive(Deserialize, Default)]
struct StartServerRequest {
    #[serde(default)]
    checkout_root: Option<String>,
    #[serde(default)]
    venv_python: Option<String>,
    #[serde(default)]
    runtime_dir: Option<String>,
}

async fn start_server(
    State(state): State<AppState>,
    Path(server_id): Path<Uuid>,
    body: Option<Json<StartServerRequest>>,
) -> impl IntoResponse {
    let req = body.map(|Json(r)| r).unwrap_or_default();
    // All three or none: a half-specified checkout is a mistake, not a
    // request to guess the other two.
    let started = match (req.checkout_root, req.venv_python, req.runtime_dir) {
        (Some(root), Some(python), Some(run)) => state.core.start_server(server_id, root, python, run).await,
        _ => state.core.start_server_here(server_id).await,
    };
    match started {
        Ok(()) => match state.core.get_server(server_id) {
            Ok(server) => Json(server).into_response(),
            Err(err) => core_error_to_status(err).into_response(),
        },
        Err(err) => core_error_to_status(err).into_response(),
    }
}

async fn stop_server(State(state): State<AppState>, Path(server_id): Path<Uuid>) -> impl IntoResponse {
    match state.core.stop_server(server_id).await {
        Ok(()) => match state.core.get_server(server_id) {
            Ok(server) => Json(server).into_response(),
            Err(err) => core_error_to_status(err).into_response(),
        },
        Err(err) => core_error_to_status(err).into_response(),
    }
}

// --- databases -------------------------------------------------------------

async fn list_databases_for_server(
    State(state): State<AppState>,
    Path(server_id): Path<Uuid>,
) -> impl IntoResponse {
    match state.core.list_databases(Some(server_id)) {
        Ok(dbs) => Json(dbs).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

async fn list_all_databases(State(state): State<AppState>) -> impl IntoResponse {
    match state.core.list_databases(None) {
        Ok(dbs) => Json(dbs).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

#[derive(Deserialize)]
struct CreateDatabaseRequest {
    name: String,
    /// Modules to have installed when the database is handed over. Empty
    /// (the default) keeps the old behaviour: a bare Postgres database that
    /// no Odoo has touched yet.
    #[serde(default)]
    modules: Vec<String>,
}

async fn create_database(
    State(state): State<AppState>,
    Path(server_id): Path<Uuid>,
    Json(req): Json<CreateDatabaseRequest>,
) -> impl IntoResponse {
    match state.core.create_database_with_modules(server_id, req.name, &req.modules).await {
        // `from_cache` is reported rather than hidden: a UI that says
        // "instant" when it actually spent a minute booting Odoo is a small
        // lie, and the difference is exactly what the user just waited for.
        Ok((db, from_cache)) => {
            (StatusCode::CREATED, Json(serde_json::json!({ "database": db, "from_cache": from_cache }))).into_response()
        }
        Err(err) => core_error_to_status(err).into_response(),
    }
}

/// The pre-initialized databases kept so the next one of a given shape is a
/// copy rather than a boot.
async fn list_templates(State(state): State<AppState>) -> impl IntoResponse {
    match state.core.cached_templates().await {
        Ok(templates) => Json(templates).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

/// Throws the cache away. Costs only time — every template rebuilds itself
/// the next time something needs it.
async fn clear_templates(State(state): State<AppState>) -> impl IntoResponse {
    match state.core.clear_template_cache().await {
        Ok(freed_bytes) => Json(serde_json::json!({ "freed_bytes": freed_bytes })).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

#[derive(Deserialize)]
struct DiscoveryQuery {
    path: String,
}

/// What addons folders are under a path, with a proposed load order.
/// Registers nothing — it's a suggestion to accept or argue with.
async fn discover_addons(State(state): State<AppState>, Query(q): Query<DiscoveryQuery>) -> impl IntoResponse {
    Json(state.core.discover_addons_roots(std::path::Path::new(&q.path))).into_response()
}

#[derive(Deserialize)]
struct AdoptRootsRequest {
    paths: Vec<String>,
    /// The path originally scanned, so the same classification and labels
    /// the user was shown are the ones registered — rather than being
    /// recomputed here and possibly differing from what they agreed to.
    scanned: String,
}

async fn adopt_addons_roots(
    State(state): State<AppState>,
    Path(server_id): Path<Uuid>,
    Json(req): Json<AdoptRootsRequest>,
) -> impl IntoResponse {
    let discovered = state.core.discover_addons_roots(std::path::Path::new(&req.scanned));
    let chosen: Vec<_> = discovered.into_iter().filter(|r| req.paths.contains(&r.path.to_string_lossy().to_string())).collect();
    match state.core.adopt_addons_roots(server_id, &chosen) {
        Ok(added) => (StatusCode::CREATED, Json(added)).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

/// Whether each installed module matches the code on disk — by content
/// hash, so an edit that didn't bump the manifest version still shows.
async fn module_freshness(State(state): State<AppState>, Path(database_id): Path<Uuid>) -> impl IntoResponse {
    match state.core.module_freshness(database_id).await {
        Ok(rows) => Json(rows).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

/// Updates several modules in one Odoo boot instead of one boot each.
async fn upgrade_modules_batched(
    State(state): State<AppState>,
    Path(database_id): Path<Uuid>,
    Json(req): Json<ModuleCommandRequest>,
) -> impl IntoResponse {
    match state.core.upgrade_modules_batched(database_id, &req.module_names).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

#[derive(Deserialize)]
struct UpgradeQuery {
    to: String,
}

/// What migrating this database to a newer Odoo would involve — the hop
/// chain, what each hop still has to fetch, and the name of the copy it
/// would work on. Does none of it.
async fn upgrade_plan(
    State(state): State<AppState>,
    Path(database_id): Path<Uuid>,
    Query(q): Query<UpgradeQuery>,
) -> impl IntoResponse {
    match state.core.plan_upgrade(database_id, &q.to).await {
        Ok(plan) => Json(plan).into_response(),
        // A migration OpenUpgrade can't do is a 422: the request is
        // well-formed and the answer is "that isn't possible", not "we
        // broke".
        Err(orchestrator_core::CoreError::NotConfigured(message)) => {
            (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": message }))).into_response()
        }
        Err(err) => core_error_to_status(err).into_response(),
    }
}

/// Runs the migration on a **copy**, checkpointing before each hop.
///
/// A run that stopped part way is a 200 with `failed` set, not an error:
/// the hops before it really did succeed, and the caller needs to know
/// which one stopped and which checkpoint to go back to.
async fn run_upgrade(
    State(state): State<AppState>,
    Path(database_id): Path<Uuid>,
    Query(q): Query<UpgradeQuery>,
) -> impl IntoResponse {
    match state.core.run_upgrade(database_id, &q.to).await {
        Ok(run) => Json(run).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

/// The versions OpenUpgrade can migrate to, newest first.
async fn upgrade_targets() -> impl IntoResponse {
    Json(orchestrator_core::openupgrade::MIGRATABLE_TO).into_response()
}

async fn reconcile_databases(State(state): State<AppState>) -> impl IntoResponse {
    match state.core.reconcile_databases().await {
        Ok(report) => Json(report).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

#[derive(Serialize)]
struct RefreshSizesResponse {
    updated: u32,
}

async fn refresh_database_sizes(State(state): State<AppState>) -> impl IntoResponse {
    match state.core.refresh_database_sizes().await {
        Ok(updated) => Json(RefreshSizesResponse { updated }).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

#[derive(Deserialize)]
struct AdoptDatabaseRequest {
    name: String,
}

async fn adopt_database(
    State(state): State<AppState>,
    Path(server_id): Path<Uuid>,
    Json(req): Json<AdoptDatabaseRequest>,
) -> impl IntoResponse {
    match state.core.adopt_database(server_id, req.name).await {
        Ok(db) => (StatusCode::CREATED, Json(db)).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

/// Removes the row, not the database — see `Core::forget_database`. A
/// POST rather than a DELETE precisely so it can't be confused with
/// `DELETE /databases/:id`, which really does drop data.
async fn forget_database(State(state): State<AppState>, Path(database_id): Path<Uuid>) -> impl IntoResponse {
    match state.core.forget_database(database_id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

async fn disk_usage(State(state): State<AppState>) -> impl IntoResponse {
    match state.core.disk_usage() {
        Ok(usage) => Json(usage).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

/// Real memory and CPU per running Odoo, measured now. Absent entries
/// mean "not running", never zero.
async fn process_usage(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.core.running_odoo_usage()).into_response()
}

#[derive(Deserialize)]
struct LogsQuery {
    #[serde(default = "default_log_limit")]
    limit: usize,
}

fn default_log_limit() -> usize {
    1000
}

/// The output this Odoo has produced since the app started it.
///
/// An empty array is a real answer, not an error: the buffer lives in
/// memory, so an Odoo started by an earlier run of the app has none here.
async fn server_logs(
    State(state): State<AppState>,
    Path(server_id): Path<Uuid>,
    Query(q): Query<LogsQuery>,
) -> impl IntoResponse {
    Json(state.core.recent_logs(server_id, q.limit.min(5000))).into_response()
}

#[derive(Deserialize)]
struct BranchQuery {
    branch: String,
}

/// The snapshots taken on one branch. An empty array means this branch
/// has none yet — an ordinary state for a branch you just created.
async fn snapshots_on_branch(
    State(state): State<AppState>,
    Path(server_id): Path<Uuid>,
    Query(q): Query<BranchQuery>,
) -> impl IntoResponse {
    match state.core.snapshots_on_branch(server_id, &q.branch) {
        Ok(found) => Json(found).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

/// What this Odoo should do when code on disk changes.
///
/// `PATCH` rather than `PUT` because the two switches are independent and a
/// caller flipping one should not have to know the state of the other.
#[derive(Deserialize)]
struct ReloadPolicyRequest {
    reload_python: Option<bool>,
    update_on_data_change: Option<bool>,
}

async fn set_reload_policy(
    State(state): State<AppState>,
    Path(server_id): Path<Uuid>,
    Json(body): Json<ReloadPolicyRequest>,
) -> impl IntoResponse {
    let current = match state.core.get_server(server_id) {
        Ok(server) => server.reload,
        Err(err) => return core_error_to_status(err).into_response(),
    };
    let policy = ReloadPolicy {
        reload_python: body.reload_python.unwrap_or(current.reload_python),
        update_on_data_change: body.update_on_data_change.unwrap_or(current.update_on_data_change),
    };
    match state.core.set_reload_policy(server_id, policy).await {
        Ok(server) => Json(server).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

/// Exactly what starting this server would run — the generated `odoo.conf`
/// *and* the command line, rendered by the same code that starts it, so the
/// UI can't show something different from what runs. Both halves matter:
/// Odoo takes `dbfilter` only from the file and `--dev` only from the flags.
async fn preview_odoo_conf(State(state): State<AppState>, Path(server_id): Path<Uuid>) -> impl IntoResponse {
    match state.core.preview_odoo_conf(server_id) {
        Ok(preview) => Json(preview).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

async fn list_odoo_copies(State(state): State<AppState>) -> impl IntoResponse {
    match state.core.cached_odoo_copies() {
        Ok(copies) => Json(copies).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

#[derive(Serialize)]
struct DeleteOdooCopyResponse {
    freed_bytes: u64,
}

async fn delete_odoo_copy(State(state): State<AppState>, Path(folder): Path<String>) -> impl IntoResponse {
    match state.core.delete_cached_odoo(&folder) {
        Ok(freed_bytes) => Json(DeleteOdooCopyResponse { freed_bytes }).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

#[derive(Deserialize)]
struct DuplicateDatabaseRequest {
    new_name: String,
}

async fn duplicate_database(
    State(state): State<AppState>,
    Path(database_id): Path<Uuid>,
    Json(req): Json<DuplicateDatabaseRequest>,
) -> impl IntoResponse {
    match state.core.duplicate_database(database_id, req.new_name).await {
        Ok(db) => (StatusCode::CREATED, Json(db)).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

async fn drop_database(State(state): State<AppState>, Path(database_id): Path<Uuid>) -> impl IntoResponse {
    match state.core.drop_database(database_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

#[derive(Serialize)]
struct BackupDatabaseResponse {
    path: String,
}

async fn backup_database(State(state): State<AppState>, Path(database_id): Path<Uuid>) -> impl IntoResponse {
    match state.core.backup_database(database_id, &*state.backups_dir).await {
        Ok(path) => Json(BackupDatabaseResponse { path: path.to_string_lossy().to_string() }).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

#[derive(Deserialize)]
struct RestoreDatabaseRequest {
    new_name: String,
    dump_file: String,
    /// Run Odoo's neutralization as part of the restore. Defaults to
    /// **true**: a caller that forgets to ask should get the safe copy, not
    /// a live one. Opting out has to be deliberate.
    #[serde(default = "default_true")]
    neutralize: bool,
}

fn default_true() -> bool {
    true
}

/// Every backup on disk. Reads the folder the API was configured with, so
/// a dump the user moved or deleted outside the app is simply not offered
/// rather than listed and then failing on restore.
async fn list_backups(State(state): State<AppState>) -> impl IntoResponse {
    match state.core.list_backups(&*state.backups_dir) {
        Ok(backups) => Json(backups).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

/// Runs Odoo's own neutralization over one database: mail servers off and
/// their credentials wiped, crons stopped, payment provider credentials
/// cleared. The single most important thing to do to a copy of a client's
/// production database before touching it.
async fn neutralize_database(State(state): State<AppState>, Path(database_id): Path<Uuid>) -> impl IntoResponse {
    match state.core.neutralize_database_here(database_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

/// Whether each database is neutralized, read live from the databases
/// themselves rather than from anything this app remembers — so a database
/// neutralized outside the app, or restored from an already-neutralized
/// dump, reads correctly.
async fn neutralization_states(State(state): State<AppState>) -> impl IntoResponse {
    match state.core.neutralization_states().await {
        Ok(states) => {
            let map: std::collections::HashMap<String, _> = states.into_iter().map(|(id, s)| (id.to_string(), s)).collect();
            Json(map).into_response()
        }
        Err(err) => core_error_to_status(err).into_response(),
    }
}

async fn restore_database(
    State(state): State<AppState>,
    Path(server_id): Path<Uuid>,
    Json(req): Json<RestoreDatabaseRequest>,
) -> impl IntoResponse {
    let restored = match state.core.restore_database(server_id, req.new_name, req.dump_file).await {
        Ok(db) => db,
        Err(err) => return core_error_to_status(err).into_response(),
    };
    // Neutralization is part of the restore, not a follow-up the caller
    // might forget: the window between "a client's production data is now
    // on this machine" and "it can no longer email their customers" should
    // not be a round trip wide.
    if req.neutralize {
        if let Err(err) = state.core.neutralize_database_here(restored.id).await {
            tracing::error!("restored {} but could not neutralize it: {err}", restored.name);
            return (
                StatusCode::CREATED,
                Json(serde_json::json!({
                    "database": restored,
                    "neutralized": false,
                    "warning": "The database was restored but neutralization failed — treat it as live data until you have run it manually.",
                })),
            )
                .into_response();
        }
    }
    (StatusCode::CREATED, Json(serde_json::json!({ "database": restored, "neutralized": req.neutralize }))).into_response()
}

// --- snapshots (task 3.1) -------------------------------------------------
//
// The database half reuses `duplicate_database`'s exact mechanism (see
// `Core::create_snapshot`'s doc comment); the filestore half is new
// (`orchestrator_core::filestore`). `filestore_source`, when given, is an
// absolute path on this machine the shell process can read — there's no
// real Odoo filestore anywhere in this codebase yet (see `odoo.rs`'s module
// doc comment), so in practice this is `None` for every snapshot taken
// today, and nothing here pretends otherwise.

#[derive(Deserialize)]
struct CreateSnapshotRequest {
    name: String,
    filestore_source: Option<String>,
}

async fn create_snapshot(
    State(state): State<AppState>,
    Path(database_id): Path<Uuid>,
    Json(req): Json<CreateSnapshotRequest>,
) -> impl IntoResponse {
    // Passing `None` for both is now the *normal* case, and means "take
    // the whole database, files included" — the core derives where this
    // database's filestore is rather than making every caller remember to
    // ask for it. `filestore_source` stays accepted for a caller with a
    // path of its own; when it's given, the app's snapshots folder is the
    // destination as before.
    let filestore_source = req.filestore_source.map(std::path::PathBuf::from);
    let snapshots_dir = filestore_source.as_ref().map(|_| (*state.snapshots_dir).clone());
    match state.core.create_snapshot(database_id, req.name, filestore_source, snapshots_dir).await {
        Ok(snapshot) => (StatusCode::CREATED, Json(snapshot)).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

async fn list_snapshots_for_database(State(state): State<AppState>, Path(database_id): Path<Uuid>) -> impl IntoResponse {
    match state.core.list_snapshots(Some(database_id)) {
        Ok(snapshots) => Json(snapshots).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

async fn list_all_snapshots(State(state): State<AppState>) -> impl IntoResponse {
    match state.core.list_snapshots(None) {
        Ok(snapshots) => Json(snapshots).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

async fn delete_snapshot(State(state): State<AppState>, Path(snapshot_id): Path<Uuid>) -> impl IntoResponse {
    match state.core.delete_snapshot(snapshot_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

// --- revert to snapshot (task 3.3) ----------------------------------------
//
// Overwrites the live database named in the URL, in place, from an existing
// snapshot's frozen copy — see `Core::revert_database_to_snapshot`'s doc
// comment for exactly how this differs from `create_snapshot`/
// `delete_snapshot` above and from `restore_database`. `filestore_target`,
// like `create_snapshot`'s `filestore_source`, is an absolute path on this
// machine; omit it for a Postgres-only revert (the common case today, same
// reasoning as the rest of this file's snapshot handlers).

#[derive(Deserialize)]
struct RevertDatabaseRequest {
    snapshot_id: Uuid,
    counter_snapshot_name: Option<String>,
    filestore_target: Option<String>,
}

#[derive(Serialize)]
struct RevertDatabaseResponse {
    counter_snapshot: Option<orchestrator_core::Snapshot>,
}

async fn revert_database_to_snapshot(
    State(state): State<AppState>,
    Path(database_id): Path<Uuid>,
    Json(req): Json<RevertDatabaseRequest>,
) -> impl IntoResponse {
    let filestore_target = req.filestore_target.map(std::path::PathBuf::from);
    match state
        .core
        .revert_database_to_snapshot(database_id, req.snapshot_id, req.counter_snapshot_name, filestore_target)
        .await
    {
        Ok(counter_snapshot) => Json(RevertDatabaseResponse { counter_snapshot }).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

// --- git-branch binding (task 3.2) ----------------------------------------
//
// A best-effort prefill suggestion, not a requirement — see
// `Core::current_git_branch`'s doc comment. `branch: null` is a completely
// normal, expected response (no linked repo yet, or it isn't on a
// resolvable branch), not an error condition the frontend needs to treat
// specially.

#[derive(Serialize)]
struct GitBranchResponse {
    branch: Option<String>,
}

async fn current_git_branch(State(state): State<AppState>, Path(server_id): Path<Uuid>) -> impl IntoResponse {
    match state.core.current_git_branch(server_id) {
        Ok(branch) => Json(GitBranchResponse { branch }).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

// --- addons sources & module scanning ------------------------------------
//
// Workstream 1 (odoo-orchestrator-task-breakdown.md): manifest parsing and
// shadowing detection live in orchestrator-core (manifest.rs, modules.rs);
// these handlers are thin wiring, plus the one thing worth doing here rather
// than in core — running the filesystem walk on a blocking thread so a large
// addons-path (an OCA mono-repo can have hundreds of modules) can't stall the
// same async runtime the WebSocket broadcast loop shares.

#[derive(Deserialize)]
struct CreateAddonsSourceRequest {
    label: String,
    path_or_url: String,
    kind: SourceKind,
    rank: u32,
}

async fn create_addons_source(
    State(state): State<AppState>,
    Path(server_id): Path<Uuid>,
    Json(req): Json<CreateAddonsSourceRequest>,
) -> impl IntoResponse {
    // `add_modules_from` clones when handed a git URL and uses the value
    // as-is when handed a path, so "my modules are on GitHub" and "my
    // modules are in this folder" are the same request here.
    match state.core.add_modules_from(server_id, req.label, req.path_or_url, req.kind, req.rank).await {
        Ok(source) => (StatusCode::CREATED, Json(source)).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

async fn list_addons_sources(State(state): State<AppState>, Path(server_id): Path<Uuid>) -> impl IntoResponse {
    match state.core.list_addons_sources(server_id) {
        Ok(sources) => Json(sources).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

async fn scan_modules(State(state): State<AppState>, Path(server_id): Path<Uuid>) -> impl IntoResponse {
    let core = state.core.clone();
    match tokio::task::spawn_blocking(move || core.scan_modules(server_id)).await {
        Ok(Ok(scan)) => Json(scan).into_response(),
        Ok(Err(err)) => core_error_to_status(err).into_response(),
        Err(join_err) => {
            tracing::error!("scan_modules task panicked: {join_err}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// --- module install/upgrade/uninstall (task 2.4) --------------------------
//
// Same posture as the server-start request above: `checkout_root`,
// `venv_python`, and `runtime_dir` are absolute local paths, not uploads —
// this is a local-desktop app. `module_names` is a real technical-name
// list (e.g. `["sale", "note"]`), same shape `scan_modules`'s own results
// (`ModuleListing::technical_name`) already surface.

/// The paths are optional, and omitting them is the normal case: the core
/// resolves this Odoo's own checkout, interpreter and working directory
/// itself. They stayed *required* for far too long, which meant the UI —
/// which has no way to know a filesystem path — could not install a module
/// at all; every call failed deserialization on a missing field.
#[derive(Deserialize)]
struct ModuleCommandRequest {
    module_names: Vec<String>,
    #[serde(default)]
    checkout_root: Option<String>,
    #[serde(default)]
    venv_python: Option<String>,
    #[serde(default)]
    runtime_dir: Option<String>,
}

impl ModuleCommandRequest {
    /// All three or none — a half-specified checkout is a mistake, not a
    /// request to guess the rest. Same rule `start_server` already uses.
    fn explicit_paths(&self) -> Option<(String, String, String)> {
        match (&self.checkout_root, &self.venv_python, &self.runtime_dir) {
            (Some(root), Some(python), Some(run)) => Some((root.clone(), python.clone(), run.clone())),
            _ => None,
        }
    }
}

async fn install_modules(
    State(state): State<AppState>,
    Path(database_id): Path<Uuid>,
    Json(req): Json<ModuleCommandRequest>,
) -> impl IntoResponse {
    let result = match req.explicit_paths() {
        Some((root, python, run)) => state.core.install_modules(database_id, &req.module_names, root, python, run).await,
        None => state.core.install_modules_here(database_id, &req.module_names).await,
    };
    match result {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

async fn upgrade_modules(
    State(state): State<AppState>,
    Path(database_id): Path<Uuid>,
    Json(req): Json<ModuleCommandRequest>,
) -> impl IntoResponse {
    let result = match req.explicit_paths() {
        Some((root, python, run)) => state.core.upgrade_modules(database_id, &req.module_names, root, python, run).await,
        None => state.core.upgrade_modules_here(database_id, &req.module_names).await,
    };
    match result {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

async fn uninstall_modules(
    State(state): State<AppState>,
    Path(database_id): Path<Uuid>,
    Json(req): Json<ModuleCommandRequest>,
) -> impl IntoResponse {
    let result = match req.explicit_paths() {
        Some((root, python, run)) => state.core.uninstall_modules(database_id, &req.module_names, root, python, run).await,
        None => state.core.uninstall_modules_here(database_id, &req.module_names).await,
    };
    match result {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

async fn database_module_states(State(state): State<AppState>, Path(database_id): Path<Uuid>) -> impl IntoResponse {
    match state.core.database_module_states(database_id).await {
        Ok(states) => Json(states).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}

// --- postgres instances ---------------------------------------------------
//
// Deliberately plural — see `PostgresInstance`'s doc comment in
// orchestrator-core. An app can register any number of these; nothing here
// (or in Core) assumes a singleton. `state` is folded into the same response
// as the persisted config because the frontend always wants both together
// (an instance row with no live-state indicator is useless), even though
// they live in different places under the hood (SQLite vs. the in-memory
// supervisor registry).

#[derive(Serialize)]
struct PostgresInstanceResponse {
    #[serde(flatten)]
    instance: orchestrator_core::PostgresInstance,
    state: orchestrator_core::PgState,
}

fn with_state(
    core: &orchestrator_core::Core,
    instance: orchestrator_core::PostgresInstance,
) -> Result<PostgresInstanceResponse, orchestrator_core::CoreError> {
    let state = core.postgres_instance_state(instance.id)?;
    Ok(PostgresInstanceResponse { instance, state })
}

async fn list_postgres_instances(State(state): State<AppState>) -> impl IntoResponse {
    let instances = match state.core.list_postgres_instances() {
        Ok(instances) => instances,
        Err(err) => return core_error_to_status(err).into_response(),
    };
    let mut out = Vec::with_capacity(instances.len());
    for instance in instances {
        match with_state(&state.core, instance) {
            Ok(resp) => out.push(resp),
            Err(err) => return core_error_to_status(err).into_response(),
        }
    }
    Json(out).into_response()
}

#[derive(Deserialize)]
struct CreatePostgresInstanceRequest {
    label: String,
    pg_version: String,
    port: u16,
    data_dir: String,
}

async fn create_postgres_instance(
    State(state): State<AppState>,
    Json(req): Json<CreatePostgresInstanceRequest>,
) -> impl IntoResponse {
    match state.core.create_postgres_instance(req.label, req.pg_version, req.port, req.data_dir) {
        Ok(instance) => match with_state(&state.core, instance) {
            Ok(resp) => (StatusCode::CREATED, Json(resp)).into_response(),
            Err(err) => core_error_to_status(err).into_response(),
        },
        Err(err) => core_error_to_status(err).into_response(),
    }
}

async fn get_postgres_instance(State(state): State<AppState>, Path(instance_id): Path<Uuid>) -> impl IntoResponse {
    match state.core.get_postgres_instance(instance_id) {
        Ok(instance) => match with_state(&state.core, instance) {
            Ok(resp) => Json(resp).into_response(),
            Err(err) => core_error_to_status(err).into_response(),
        },
        Err(err) => core_error_to_status(err).into_response(),
    }
}

// start/stop genuinely spawn and wait on subprocesses (initdb/pg_ctl), unlike
// the sync SQLite-backed handlers above — Core's own methods are already
// async and internally use spawn_blocking for the actual subprocess calls
// (see postgres.rs), so these handlers just await them directly.

async fn start_postgres_instance(State(state): State<AppState>, Path(instance_id): Path<Uuid>) -> impl IntoResponse {
    match state.core.start_postgres_instance(instance_id).await {
        Ok(()) => match state.core.postgres_instance_state(instance_id) {
            Ok(pg_state) => Json(pg_state).into_response(),
            Err(err) => core_error_to_status(err).into_response(),
        },
        Err(err) => core_error_to_status(err).into_response(),
    }
}

async fn stop_postgres_instance(State(state): State<AppState>, Path(instance_id): Path<Uuid>) -> impl IntoResponse {
    match state.core.stop_postgres_instance(instance_id).await {
        Ok(()) => match state.core.postgres_instance_state(instance_id) {
            Ok(pg_state) => Json(pg_state).into_response(),
            Err(err) => core_error_to_status(err).into_response(),
        },
        Err(err) => core_error_to_status(err).into_response(),
    }
}

// --- events -------------------------------------------------------------

#[derive(Deserialize)]
struct EventsQuery {
    limit: Option<u32>,
}

#[derive(Serialize)]
struct EventsResponse {
    events: Vec<orchestrator_core::EventEnvelope>,
}

async fn list_events(
    State(state): State<AppState>,
    Query(q): Query<EventsQuery>,
) -> impl IntoResponse {
    match state.core.recent_events(q.limit.unwrap_or(50)) {
        Ok(events) => Json(EventsResponse { events }).into_response(),
        Err(err) => core_error_to_status(err).into_response(),
    }
}
