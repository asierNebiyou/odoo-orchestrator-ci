// Typed client for orchestrator-api — the local HTTP+WS API that's the
// single source of truth per the technical-design doc. Inside the real
// Tauri shell, the base URL and per-launch bearer token come from the
// shell's own `get_api_config` command (src-tauri/src/main.rs) — the shell
// generates a fresh random token every launch, so this can't be a
// build-time constant. Outside Tauri (`npm run dev` in a plain browser,
// e.g. for the e2e Playwright scripts), it falls back to the Vite env vars
// in `frontend/.env.local`, which must be pointed at a separately-running
// headless `orchestrator-shell` (see README.md).

interface ApiConfig {
  baseUrl: string;
  token: string;
}

let configPromise: Promise<ApiConfig> | null = null;

function resolveConfig(): Promise<ApiConfig> {
  if (!configPromise) {
    configPromise = isTauri()
      ? invokeGetApiConfig()
      : Promise.resolve({
          baseUrl: import.meta.env.VITE_API_BASE_URL ?? "http://127.0.0.1:0",
          token: import.meta.env.VITE_API_TOKEN ?? "",
        });
  }
  return configPromise;
}

function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

async function invokeGetApiConfig(): Promise<ApiConfig> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<ApiConfig>("get_api_config");
}

export type ServerState =
  | { status: "stopped" }
  | { status: "starting" }
  | { status: "running" }
  | { status: "stopping" }
  | { status: "crashed"; exit_code: number };

/** The top level of the UI — a named place to keep a client's or a
 *  side-project's Odoos together. Purely organizational: a project owns no
 *  port, no Postgres instance and no addons path, so moving an Odoo between
 *  projects changes nothing about how it runs. `color` indexes the six-swatch
 *  palette in `theme.css` and is stored rather than derived, so renaming a
 *  project never re-colours it. */
export interface Project {
  id: string;
  name: string;
  color: number;
  created_at: string;
}

export interface OdooServer {
  id: string;
  /** Which project this Odoo is filed under. Null only for rows written
   *  before projects existed; the core adopts those into a default project
   *  on startup, so in practice this is always set. */
  project_id: string | null;
  name: string;
  odoo_version: string;
  port: number;
  postgres_instance_id: string;
  state: ServerState;
  /** What this Odoo does when code on disk changes. Both switches are off
   *  until somebody turns them on — restarting an Odoo nobody asked to be
   *  restarted is the surprise this app exists to avoid. */
  reload: ReloadPolicy;
  started_at: string | null;
  created_at: string;
}

/** Whether a database can still reach the outside world.
 *
 *  `unknown` is deliberately not `live`: "its Postgres isn't running so I
 *  couldn't look" must never render as either "it's fine" or "it's
 *  dangerous". */
export type NeutralizationState = "neutralized" | "live" | "not_odoo" | "unknown";

/** Where a database's contents came from — which is what decides whether
 *  it might hold somebody's real data. */
export type DatabaseOrigin = "created" | "duplicated" | "restored" | "adopted";

export interface RestoreResult {
  database: Database;
  neutralized: boolean;
  /** Present only when the restore succeeded and the neutralization did
   *  not — the one outcome a caller must not read as success. */
  warning?: string;
}

export type Freshness = "live" | "changed" | "unrecorded" | "not_on_disk";

export interface ModuleFreshness {
  technical_name: string;
  freshness: Freshness;
}

export interface DiscoveredRoot {
  path: string;
  label: string;
  kind: SourceKind;
  module_count: number;
  sample: string[];
  /** Why this app thinks it's that kind. Shown, because a guess nobody can
   *  check is a guess they have to trust. */
  why: string;
}

export interface UpgradeHop {
  from: string;
  to: string;
  /** Whether the pieces this hop needs are already on disk. `false` isn't a
   *  blocker — it's a download — but it's the difference between starting
   *  now and starting in ten minutes. */
  odoo_ready: boolean;
  openupgrade_ready: boolean;
}

export interface CarriedAddon {
  label: string;
  kind: SourceKind;
  path_or_url: string;
}

export interface UpgradePlan {
  database_id: string;
  database_name: string;
  from: string;
  to: string;
  /** The copy it would work on. The original is never touched. */
  into: string;
  hops: UpgradeHop[];
  /** The server's own Private/OCA addons sources that ride along on every
   *  hop's addons path. Carrying the code isn't the same as migrating it —
   *  OpenUpgrade only ships real migration scripts for Odoo's own core and
   *  OCA-tracked modules, so a private module just gets re-initialized like
   *  any addon. Shown so that's a visible bet, not a silent assumption. */
  carried_addons: CarriedAddon[];
}

export interface UpgradeRun {
  plan: UpgradePlan;
  working_database_id: string | null;
  completed: UpgradeHop[];
  failed: {
    hop: UpgradeHop;
    message: string;
    /** The snapshot taken immediately before the hop that failed. */
    checkpoint_id: string;
  } | null;
}

export interface CachedTemplate {
  key: string;
  database_name: string;
  instance_id: string;
  size_bytes: number;
}

export interface Backup {
  /** Absolute path of the dump, and the handle for restoring it. */
  path: string;
  /** The database it came from, read out of the filename. */
  database_name: string;
  database_id: string | null;
  /** Whether that database still exists. `false` is the case this list is
   *  really for. */
  source_still_exists: boolean;
  /** Whether the attachments were taken too — restoring rows without the
   *  files hands someone a database of attachments that 404. */
  has_filestore: boolean;
  size_bytes: number;
  taken_at: string;
}

export interface StartupPreview {
  /** The generated `odoo.conf`, verbatim. */
  conf: string;
  /** The command line it is passed to. */
  command: string;
}

export interface ReloadPolicy {
  /** Hands `.py` reloading to Odoo's own `--dev=reload`, which compiles a
   *  change before acting on it. Takes effect on next start. */
  reload_python: boolean;
  /** This app's own watcher: runs `-u <module>` when that module's data
   *  XML/CSV changes, which is the one case Odoo has no answer for. */
  update_on_data_change: boolean;
}

export interface Database {
  id: string;
  server_id: string;
  name: string;
  /** How it came to exist. `restored` and `adopted` are the ones that
   *  might hold somebody else's real data. */
  origin: DatabaseOrigin;
  /** The hostname this database answers on; null means the default,
   *  `<name>.odoo`. Odoo's own dbfilter routes on the first label of the
   *  Host header, which is what lets one Odoo serve several databases —
   *  a custom domain works because the proxy rewrites Host before
   *  forwarding, not because Odoo is reconfigured. */
  domain: string | null;
  size_bytes: number;
  last_backup_at: string | null;
  created_at: string;
}

export type PgState =
  | { status: "stopped" }
  | { status: "starting" }
  | { status: "running"; pid: number }
  | { status: "stopping" }
  | { status: "crashed"; exit_code: number | null };

// Deliberately not a singleton — see `PostgresInstance`'s doc comment in
// orchestrator-core/src/model.rs. An app can have any number of these
// (different Postgres major versions under test, or a user who just wants
// isolation), so this list-shaped API is the point, not an interim step
// toward one.
export interface PostgresInstance {
  id: string;
  label: string;
  pg_version: string;
  port: number;
  data_dir: string;
  created_at: string;
  state: PgState;
}

// Task 3.1's primitive — an instant, named copy of a database (and,
// optionally, a filestore directory) taken at a point in time. Deliberately
// keyed to a free-form `name`, not necessarily a git branch — 3.2 is what
// eventually binds one to a branch automatically.
export interface Snapshot {
  id: string;
  server_id: string;
  database_id: string;
  source_database_name: string;
  snapshot_database_name: string;
  name: string;
  filestore_snapshot_path: string | null;
  /** The git branch the linked repo was on when this was taken, when
   *  there was one to read. Recorded, never inferred from `name` — people
   *  rename snapshots, and a label that looks like a branch is not
   *  evidence that it is one. */
  git_branch: string | null;
  created_at: string;
}

/** One row of the proxy's routing table. Both supported shapes show up
 *  here identically — several routes sharing a `port` is one Odoo serving
 *  several databases; a route with a port to itself is one Odoo per
 *  database. */
export interface Route {
  host: string;
  port: number;
  database: string;
  server_id: string;
  database_id: string;
  running: boolean;
}

export interface EventEnvelope {
  id: string;
  occurred_at: string;
  /** How long the operation took, for the ones that take real time.
   *  Absent on instant state changes — and absent on everything recorded
   *  before this was measured, which is why anything computed from it
   *  must show its own sample size rather than a bare average. */
  duration_ms?: number;
  type: string;
  payload: Record<string, unknown>;
}

/** A database as Postgres itself reports it, which may or may not be one
 *  this app knows about. */
export interface ClusterDatabase {
  instance_id: string;
  name: string;
  size_bytes: number;
}

export interface MissingDatabase {
  database_id: string;
  name: string;
  server_id: string;
  store: string;
}

/** The difference between what the app thinks exists and what does.
 *  `unchecked` lists stores that couldn't be asked — their databases are
 *  deliberately *not* reported as missing, because "I couldn't look" and
 *  "it's gone" must never look the same. */
export interface Reconciliation {
  untracked: ClusterDatabase[];
  missing: MissingDatabase[];
  unchecked: string[];
}

/** One copy of Odoo in the app's own cache folder. */
export interface CachedRuntime {
  version: string;
  folder: string;
  path: string;
  size_bytes: number;
  has_venv: boolean;
}

/** Real memory and CPU for one running Odoo, measured from the OS at the
 *  moment it was asked for. An Odoo that isn't running has no entry —
 *  never a zero, which would read as "using nothing" rather than "not
 *  there". */
export interface ProcessUsage {
  memory_bytes: number;
  /** Percent of one CPU as the OS reports it — averaged over the
   *  process's life on Linux, not an instantaneous reading. */
  cpu_percent: number;
}

/** One line of real output from a supervised Odoo.
 *
 *  Deliberately not an event: the event log records what the *app* did,
 *  and burying it under thousands of Odoo log lines would ruin the thing
 *  that makes Activity readable. `stream` is "stdout", "stderr", or
 *  "notice" — the last being the server telling us it had to drop lines,
 *  so a gap in a traceback is never mistaken for the end of one. */
export interface LogLine {
  server_id: string;
  at: string;
  stream: string;
  line: string;
}

export interface DiskUsage {
  database_bytes: number;
  snapshot_filestore_bytes: number;
  backup_bytes: number;
  module_bytes: number;
  odoo_copy_bytes: number;
  odoo_copies: CachedRuntime[];
}

export type SourceKind = "private" | "oca" | "core";

export interface AddonsSource {
  id: string;
  server_id: string;
  label: string;
  path_or_url: string;
  kind: SourceKind;
  rank: number;
}

export interface ModuleListing {
  technical_name: string;
  name: string;
  version: string | null;
  category: string | null;
  application: boolean;
  installable: boolean;
  auto_install_enabled: boolean;
  depends: string[];
  source_id: string;
  source_label: string;
  source_kind: SourceKind;
  rank: number;
  shadowed: boolean;
  path: string;
}

export interface ShadowedCopy {
  source_label: string;
  source_kind: SourceKind;
  rank: number;
}

export interface Collision {
  technical_name: string;
  winner_source_label: string;
  winner_rank: number;
  shadowed: ShadowedCopy[];
}

export interface UnresolvedDependency {
  technical_name: string;
  missing_dependency: string;
}

export interface SourceScanError {
  source_id: string;
  source_label: string;
  path_or_url: string;
  message: string;
}

export interface ManifestParseIssue {
  source_label: string;
  technical_name: string;
  path: string;
  message: string;
}

// Task 2.4/2.5's real per-database module state — sourced straight from the
// database's own `ir_module_module` table (see `Core::database_module_states`),
// not derived from the filesystem scan above. `installed_version === null`
// means never installed. To learn whether it's genuinely up to date,
// compare against the *scanned* module's own `version` (from `ModuleScan`
// above, sourced independently via this project's own manifest parser,
// not Odoo's ORM) for the same `technical_name` — see
// `DatabaseModuleState`'s own doc comment on the Rust side for the real,
// confusing Odoo column-naming trap this works around.
export interface DatabaseModuleState {
  technical_name: string;
  state: string;
  installed_version: string | null;
}

export interface ModuleScan {
  server_id: string;
  scanned_at: string;
  modules: ModuleListing[];
  collisions: Collision[];
  unresolved_depends: UnresolvedDependency[];
  install_order: string[] | null;
  cycle: string[] | null;
  source_errors: SourceScanError[];
  parse_errors: ManifestParseIssue[];
}

class ApiError extends Error {
  constructor(public status: number, message: string) {
    super(message);
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const { baseUrl, token } = await resolveConfig();
  const res = await fetch(`${baseUrl}${path}`, {
    ...init,
    headers: {
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
      "Content-Type": "application/json",
      ...init?.headers,
    },
  });
  if (!res.ok) {
    throw new ApiError(res.status, `${init?.method ?? "GET"} ${path} failed: ${res.status}`);
  }
  if (res.status === 204) {
    return undefined as T;
  }
  return (await res.json()) as T;
}

export const api = {
  listProjects: () => request<Project[]>("/projects"),

  createProject: (name: string, color: number) =>
    request<Project>("/projects", { method: "POST", body: JSON.stringify({ name, color }) }),

  updateProject: (projectId: string, name: string, color: number) =>
    request<Project>(`/projects/${projectId}`, { method: "PATCH", body: JSON.stringify({ name, color }) }),

  // Refused with a 409 while the project still holds Odoos — deliberately
  // not a cascade. See `Core::delete_project`'s doc comment: a one-button
  // action that quietly performed several rung-4 deletions at once is the
  // exact blast-radius mismatch the friction ladder exists to prevent.
  deleteProject: (projectId: string) => request<void>(`/projects/${projectId}`, { method: "DELETE" }),

  // Copies the project's *setup* — each Odoo's version, Postgres instance
  // and addons path, each on a port nothing else claims. Databases are not
  // copied; say so wherever this is offered.
  duplicateProject: (projectId: string, newName: string) =>
    request<Project>(`/projects/${projectId}/duplicate`, {
      method: "POST",
      body: JSON.stringify({ new_name: newName }),
    }),

  /** Whether a version of Odoo is already on this machine — from the
   *  app's cache or a checkout it found — so the UI can say "reused"
   *  rather than downloading a gigabyte without warning. */
  findOdoo: (version: string) => request<{ found: boolean; path: string | null; source: string | null }>(
    `/odoo-runtimes/${encodeURIComponent(version)}`,
  ),

  /** Every hostname the proxy answers on and where each goes. */
  listRoutes: () => request<Route[]>("/routes"),

  /** Give a database its own hostname; null restores `<name>.localhost`.
   *  409s if another database already answers on it. */
  setDatabaseDomain: (databaseId: string, domain: string | null) =>
    request<Database>(`/databases/${databaseId}/domain`, {
      method: "PUT",
      body: JSON.stringify({ domain }),
    }),

  listServers: () => request<OdooServer[]>("/servers"),

  // Every server needs a `postgres_instance_id` up front — see the topology
  // rule in `odoo-orchestrator-runtime-architecture.md` (share by default
  // when nothing forces separation, a dedicated instance only when
  // something genuinely does). Callers pick which instance; nothing here
  // defaults to "the only one".
  // No storage argument: the core uses (or creates) the app's own place
  // to keep databases. Nobody should have to answer "where should this be
  // stored" to make an Odoo.
  createServer: (name: string, odooVersion: string, port: number, projectId?: string) =>
    request<OdooServer>("/servers", {
      method: "POST",
      body: JSON.stringify({ name, odoo_version: odooVersion, port, project_id: projectId ?? null }),
    }),

  // Task 2.2 — genuinely spawns/stops real `odoo-bin` server-side, via
  // `OdooSupervisor`. `checkoutRoot` (the directory containing `odoo-bin`)
  // and `venvPython` (an already-provisioned interpreter with that
  // checkout's requirements installed — this app doesn't provision one
  // inline, see `Core::start_server`'s doc comment) are absolute local
  // paths, same posture as `restoreDatabase`'s `dumpFile` below.
  // `runtimeDir` is where this server's real `odoo.conf` and filestore get
  // written. Can take real time (Postgres role/schema setup, then the full
  // registry bootstrap), unlike the instant CRUD calls above.
  // Takes nothing but the id — the core finds the right copy of Odoo
  // itself, fetching that version if this machine hasn't got it. Can
  // therefore take real time on a first run.
  startServer: (serverId: string) => request<OdooServer>(`/servers/${serverId}/start`, { method: "POST" }),

  stopServer: (serverId: string) => request<OdooServer>(`/servers/${serverId}/stop`, { method: "POST" }),

  listDatabases: (serverId?: string) =>
    request<Database[]>(serverId ? `/servers/${serverId}/databases` : "/databases"),

  // Genuinely creates the database on the server's Postgres instance
  // (createdb over a real connection) — not just a SQLite metadata row.
  /** Creates a database, optionally with modules already installed.
   *
   *  With modules, the first one of a given shape pays a real Odoo boot and
   *  the result is kept as a template; every later one is a copy, which is
   *  seconds. `from_cache` says which of those just happened — reported
   *  rather than hidden, because claiming "instant" after a minute of
   *  booting is a small lie. */
  createDatabase: (serverId: string, name: string, modules: string[] = []) =>
    request<{ database: Database; from_cache: boolean }>(`/servers/${serverId}/databases`, {
      method: "POST",
      body: JSON.stringify({ name, modules }),
    }),

  /** The addons folders under a path, with a proposed load order and a
   *  stated reason for each guess. Registers nothing — a suggestion to
   *  accept or argue with. */
  discoverAddons: (path: string) =>
    request<DiscoveredRoot[]>(`/addons-discovery?path=${encodeURIComponent(path)}`),

  /** Registers the chosen folders in the order they were proposed. */
  adoptAddonsRoots: (serverId: string, scanned: string, paths: string[]) =>
    request<AddonsSource[]>(`/servers/${serverId}/addons-sources/adopt`, {
      method: "POST",
      body: JSON.stringify({ scanned, paths }),
    }),

  /** Whether each installed module matches the code on disk — by content
   *  hash, so an edit that didn't bump the manifest version still shows.
   *  `unrecorded` is its own answer and must not be drawn as "fine". */
  moduleFreshness: (databaseId: string) =>
    request<ModuleFreshness[]>(`/databases/${databaseId}/freshness`),

  /** Updates several modules in one Odoo boot rather than one boot each. */
  upgradeModulesBatched: (databaseId: string, moduleNames: string[]) =>
    request<void>(`/databases/${databaseId}/modules/upgrade-batched`, {
      method: "POST",
      body: JSON.stringify({ module_names: moduleNames }),
    }),

  /** The versions OpenUpgrade can migrate to, newest first. */
  upgradeTargets: () => request<string[]>("/upgrade-targets"),

  /** What migrating this database would involve — the hop chain, what each
   *  hop still has to fetch, and the name of the copy it would work on.
   *  Does none of it. */
  upgradePlan: (databaseId: string, to: string) =>
    request<UpgradePlan>(`/databases/${databaseId}/upgrade-plan?to=${encodeURIComponent(to)}`),

  /** Runs it, on a copy, checkpointing before every hop. A run that stopped
   *  part way comes back with `failed` set — that's an outcome, not an
   *  error, and the hops before it really did succeed. */
  runUpgrade: (databaseId: string, to: string) =>
    request<UpgradeRun>(`/databases/${databaseId}/upgrade?to=${encodeURIComponent(to)}`, { method: "POST" }),

  /** The pre-initialized databases kept so the next one of a shape is a
   *  copy rather than a boot. */
  listTemplates: () => request<CachedTemplate[]>("/templates"),

  /** Throws the cache away. Costs only time: each template rebuilds itself
   *  the next time something needs it. */
  clearTemplates: () => request<{ freed_bytes: number }>("/templates", { method: "DELETE" }),

  // Genuine Postgres admin operations (task 2.3) — createdb/dropdb/pg_dump
  // under the hood, not simulated. All require the owning server's Postgres
  // instance to be running.
  duplicateDatabase: (databaseId: string, newName: string) =>
    request<Database>(`/databases/${databaseId}/duplicate`, {
      method: "POST",
      body: JSON.stringify({ new_name: newName }),
    }),

  dropDatabase: (databaseId: string) =>
    request<void>(`/databases/${databaseId}`, { method: "DELETE" }),

  backupDatabase: (databaseId: string) =>
    request<{ path: string }>(`/databases/${databaseId}/backup`, { method: "POST" }),

  /** Every backup this app has taken, newest first. Reads the folder on
   *  disk, so a dump moved or deleted outside the app disappears from the
   *  list instead of being offered and then failing. */
  listBackups: () => request<Backup[]>("/backups"),

  /** Restores a dump into a **new** database, neutralized by default.
   *
   *  `neutralize` defaults to true on the server too, so a caller that
   *  forgets gets the safe copy. Opting out is deliberate. The response
   *  says what actually happened, including the case where the restore
   *  worked and the neutralization didn't. */
  restoreDatabase: (serverId: string, newName: string, dumpFile: string, neutralize = true) =>
    request<RestoreResult>(`/servers/${serverId}/databases/restore`, {
      method: "POST",
      body: JSON.stringify({ new_name: newName, dump_file: dumpFile, neutralize }),
    }),

  /** Runs Odoo's own neutralization: mail servers deactivated and their
   *  passwords wiped, crons stopped, payment credentials cleared. */
  neutralizeDatabase: (databaseId: string) =>
    request<void>(`/databases/${databaseId}/neutralize`, { method: "POST" }),

  /** Whether each database is neutralized, read live out of the databases
   *  themselves — so one neutralized outside this app, or restored from an
   *  already-neutralized dump, reads correctly. Keyed by database id. */
  neutralization: () => request<Record<string, NeutralizationState>>("/neutralization"),

  // Real Postgres duplicate-via-template under the hood (reusing
  // `duplicateDatabase`'s own mechanism server-side) — a frozen copy, not a
  // live view. `filestoreSource`, when passed, must be an absolute path
  // readable by the shell process; omit it for a Postgres-only snapshot
  // (the common case today — see `Snapshot`'s doc comment on the Rust side).
  createSnapshot: (databaseId: string, name: string, filestoreSource?: string) =>
    request<Snapshot>(`/databases/${databaseId}/snapshots`, {
      method: "POST",
      body: JSON.stringify({ name, filestore_source: filestoreSource ?? null }),
    }),

  listSnapshots: (databaseId?: string) =>
    request<Snapshot[]>(databaseId ? `/databases/${databaseId}/snapshots` : "/snapshots"),

  deleteSnapshot: (snapshotId: string) =>
    request<void>(`/snapshots/${snapshotId}`, { method: "DELETE" }),

  // Task 3.3's "one-click revert" — overwrites `databaseId`'s real Postgres
  // database *in place* from `snapshotId`'s frozen copy (drop + re-template,
  // reusing the same server-side mechanism `createSnapshot` does, direction
  // reversed). Distinct from `duplicateDatabase`/`restoreDatabase` above,
  // neither of which touch an existing database's contents. Pass
  // `counterSnapshotName` to have the server take a real safety snapshot of
  // the live data immediately before the destructive part of the revert —
  // the friction-ladder's "counter-snapshot offered" element for this rung
  // (see `odoo-orchestrator-ui-design-principles.md`) — and the caller gets
  // that snapshot back so the UI can name exactly what undoes this revert.
  revertDatabaseToSnapshot: (databaseId: string, snapshotId: string, counterSnapshotName?: string) =>
    request<{ counter_snapshot: Snapshot | null }>(`/databases/${databaseId}/revert`, {
      method: "POST",
      body: JSON.stringify({
        snapshot_id: snapshotId,
        counter_snapshot_name: counterSnapshotName ?? null,
        filestore_target: null,
      }),
    }),

  // Task 3.2: a best-effort suggestion for a snapshot's name — the real
  // current branch of the server's linked ("Private"-kind) addons source,
  // when one is registered and it's genuinely a git working directory on a
  // resolvable branch. `branch: null` is a normal response (nothing to
  // suggest yet), not an error — callers should fall back to their own
  // default rather than treating a null branch as a failure.
  getGitBranch: (serverId: string) =>
    request<{ branch: string | null }>(`/servers/${serverId}/git-branch`),

  /** The snapshots taken on one branch, newest first. Empty is ordinary —
   *  a branch you just created has none. */
  snapshotsOnBranch: (serverId: string, branch: string) =>
    request<Snapshot[]>(`/servers/${serverId}/branch-snapshots?branch=${encodeURIComponent(branch)}`),

  /** Turns reloading on or off. Either field may be omitted to leave it
   *  alone. Enabling `reload_python` also installs `watchdog` into that
   *  Odoo's interpreter, because `--dev=reload` silently does nothing
   *  without it — so this call can take a few seconds the first time. */
  setReloadPolicy: (serverId: string, policy: Partial<ReloadPolicy>) =>
    request<OdooServer>(`/servers/${serverId}/reload-policy`, {
      method: "PATCH",
      body: JSON.stringify(policy),
    }),

  /** Exactly what starting this server would run, rendered by the same code
   *  that starts it. Both halves are needed: Odoo takes `dbfilter` only from
   *  the config file and `--dev` only from the command line, so showing one
   *  without the other hides half of what happens. */
  previewOdooConf: (serverId: string) => request<StartupPreview>(`/servers/${serverId}/odoo-conf`),

  recentEvents: (limit = 50) =>
    request<{ events: EventEnvelope[] }>(`/events?limit=${limit}`).then((r) => r.events),

  listAddonsSources: (serverId: string) => request<AddonsSource[]>(`/servers/${serverId}/addons-sources`),

  createAddonsSource: (serverId: string, label: string, pathOrUrl: string, kind: SourceKind, rank: number) =>
    request<AddonsSource>(`/servers/${serverId}/addons-sources`, {
      method: "POST",
      body: JSON.stringify({ label, path_or_url: pathOrUrl, kind, rank }),
    }),

  // Walks the filesystem on every call — see orchestrator-api/src/routes.rs's
  // comment on why that's on a blocking thread server-side. Can take longer
  // than the other calls here for a large addons-path, which is why the
  // Modules screen shows its own "Scanning…" state rather than treating this
  // like the instant CRUD calls above.
  scanModules: (serverId: string) => request<ModuleScan>(`/servers/${serverId}/modules`),

  // Task 2.4 — real one-shot `odoo-bin -i`/`-u`/`shell` commands per
  // database, not simulated (see `Core::install_modules`/`upgrade_modules`/
  // `uninstall_modules`). Same explicit-paths posture as `startServer`
  // above — `checkoutRoot`/`venvPython`/`runtimeDir` are plain local paths.
  // Can take real time (a fresh database's first install bootstraps the
  // whole framework).
  installModules: (databaseId: string, moduleNames: string[]) =>
    request<void>(`/databases/${databaseId}/modules/install`, {
      method: "POST",
      body: JSON.stringify({ module_names: moduleNames }),
    }),

  upgradeModules: (databaseId: string, moduleNames: string[]) =>
    request<void>(`/databases/${databaseId}/modules/upgrade`, {
      method: "POST",
      body: JSON.stringify({ module_names: moduleNames }),
    }),

  uninstallModules: (databaseId: string, moduleNames: string[]) =>
    request<void>(`/databases/${databaseId}/modules/uninstall`, {
      method: "POST",
      body: JSON.stringify({ module_names: moduleNames }),
    }),

  // Real per-database `ir_module_module` state, read straight from
  // Postgres — see `DatabaseModuleState`'s own doc comment above for what
  // "up to date" means here. An empty array is the normal, honest response
  // for a database that's never had a module installed on it, not an error.
  getDatabaseModuleStates: (databaseId: string) => request<DatabaseModuleState[]>(`/databases/${databaseId}/module-states`),

  /** Asks every running store what databases it really has and reports
   *  the difference in both directions. Real work, not a cached view. */
  reconcile: () => request<Reconciliation>("/reconciliation"),

  /** Re-measures every tracked database's size from its cluster. */
  refreshDatabaseSizes: () => request<{ updated: number }>("/maintenance/refresh-sizes", { method: "POST" }),

  /** Takes ownership of a database that already exists in a store —
   *  creates nothing. */
  adoptDatabase: (serverId: string, name: string) =>
    request<Database>(`/servers/${serverId}/databases/adopt`, {
      method: "POST",
      body: JSON.stringify({ name }),
    }),

  /** Removes the app's row for a database that's already gone from
   *  Postgres. Never touches Postgres — that's `dropDatabase`, and the two
   *  must never be offered as if they were the same button. */
  forgetDatabase: (databaseId: string) =>
    request<void>(`/databases/${databaseId}/forget`, { method: "POST" }),

  /** What this Odoo has printed since the app started it. An empty array
   *  is a real answer — the buffer is in memory, so an Odoo started by an
   *  earlier run of the app has none. */
  serverLogs: (serverId: string, limit = 1000) =>
    request<LogLine[]>(`/servers/${serverId}/logs?limit=${limit}`),

  diskUsage: () => request<DiskUsage>("/disk-usage"),

  /** Memory and CPU per running Odoo, keyed by server id. Measured now;
   *  missing keys mean "not running". */
  processUsage: () => request<Record<string, ProcessUsage>>("/process-usage"),

  listOdooCopies: () => request<CachedRuntime[]>("/odoo-copies"),

  /** Deletes one downloaded copy of Odoo. Only ever the app's own cache —
   *  a checkout adopted from elsewhere on the machine is refused. */
  deleteOdooCopy: (folder: string) =>
    request<{ freed_bytes: number }>(`/odoo-copies/${encodeURIComponent(folder)}`, { method: "DELETE" }),

  listPostgresInstances: () => request<PostgresInstance[]>("/postgres-instances"),

  createPostgresInstance: (label: string, pgVersion: string, port: number, dataDir: string) =>
    request<PostgresInstance>("/postgres-instances", {
      method: "POST",
      body: JSON.stringify({ label, pg_version: pgVersion, port, data_dir: dataDir }),
    }),

  getPostgresInstance: (instanceId: string) => request<PostgresInstance>(`/postgres-instances/${instanceId}`),

  // Genuinely start/stop a real Postgres process server-side (initdb + pg_ctl)
  // — can take a few seconds, unlike the instant CRUD calls above.
  startPostgresInstance: (instanceId: string) =>
    request<PgState>(`/postgres-instances/${instanceId}/start`, { method: "POST" }),

  stopPostgresInstance: (instanceId: string) =>
    request<PgState>(`/postgres-instances/${instanceId}/stop`, { method: "POST" }),
};

/** Live process output over /ws/logs. Returns an unsubscribe function.
 *
 *  A separate socket from the event stream on purpose: a booting Odoo
 *  emits hundreds of lines at once, and that must not starve the events
 *  the rest of the UI runs on. */
export function subscribeLogs(onLine: (l: LogLine) => void): () => void {
  let socket: WebSocket | null = null;
  let unsubscribed = false;

  resolveConfig().then(({ baseUrl, token }) => {
    if (unsubscribed) return;
    const wsUrl = `${baseUrl.replace(/^http/, "ws")}/ws/logs?token=${encodeURIComponent(token)}`;
    socket = new WebSocket(wsUrl);
    socket.onmessage = (msg) => {
      try {
        onLine(JSON.parse(msg.data) as LogLine);
      } catch (err) {
        console.error("failed to parse log line", err);
      }
    };
  });

  return () => {
    unsubscribed = true;
    socket?.close();
  };
}

/** Live event stream over /ws/events. Returns an unsubscribe function.
 *
 * The token rides as a `?token=` query param rather than an Authorization
 * header: the browser WebSocket API has no way to set custom headers on the
 * handshake, so the server checks this instead (see ws.rs) — the REST calls
 * above still use the header since fetch() can set one. */
export function subscribeEvents(onEvent: (e: EventEnvelope) => void): () => void {
  let socket: WebSocket | null = null;
  let unsubscribed = false;

  resolveConfig().then(({ baseUrl, token }) => {
    if (unsubscribed) return;
    const wsUrl = `${baseUrl.replace(/^http/, "ws")}/ws/events?token=${encodeURIComponent(token)}`;
    socket = new WebSocket(wsUrl);
    socket.onmessage = (msg) => {
      try {
        onEvent(JSON.parse(msg.data) as EventEnvelope);
      } catch (err) {
        console.error("failed to parse event envelope", err);
      }
    };
  });

  return () => {
    unsubscribed = true;
    socket?.close();
  };
}
