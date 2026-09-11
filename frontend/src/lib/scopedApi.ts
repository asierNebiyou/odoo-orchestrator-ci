// Real (not declared-only) permission enforcement for task X.1's plugin
// architecture. `createScopedApi(scopes)` wraps the real `api` singleton
// (lib/api.ts) in an object with the identical method shape, but every
// method checks the caller's granted scopes before making the real HTTP
// call — a plugin whose manifest didn't request "databases:write" cannot
// call `dropDatabase` no matter what its code says, because the wrapped
// method throws `ScopeDeniedError` before `api.dropDatabase` is ever
// reached. This is what makes the manifest's `permissions` field real: a
// plugin that lied about needing fewer permissions than it uses fails at
// the first call, loudly, in dev — not a passive label nobody checks.
//
// Screens receive one of these (see lib/plugins.ts + App.tsx) instead of
// importing the `api` singleton directly, so every screen genuinely goes
// through its own declared scopes — there's no back door via a direct
// `import { api } from "../lib/api"` once a screen is converted to a
// plugin (Topology/Engine/Modules/Activity all were, this task).
import { api, subscribeEvents as rawSubscribeEvents, type EventEnvelope } from "./api";
import type { Scope } from "./scopes";

export class ScopeDeniedError extends Error {
  constructor(
    public readonly scope: Scope,
    public readonly method: string,
  ) {
    super(`Plugin lacks permission "${scope}" required to call ${method}()`);
    this.name = "ScopeDeniedError";
  }
}

function guard<A extends unknown[], R>(needed: Scope[], method: string, granted: Set<Scope>, fn: (...args: A) => R): (...args: A) => R {
  return (...args: A): R => {
    for (const scope of needed) {
      if (!granted.has(scope)) {
        throw new ScopeDeniedError(scope, method);
      }
    }
    return fn(...args);
  };
}

/** What every api method actually requires — the source of truth `guard`
 * calls above are generated from. Exported so a permission-review UI (or a
 * test) can enumerate "what could this plugin do" from its manifest alone,
 * without duplicating this table. */
export const SCOPE_REQUIREMENTS: Record<keyof typeof api, Scope[]> = {
  listProjects: ["projects:read"],
  createProject: ["projects:write"],
  updateProject: ["projects:write"],
  deleteProject: ["projects:write"],
  // Reproduces Odoo definitions inside a new project, so it genuinely
  // writes on both sides — a caller holding only `projects:write` must not
  // be able to conjure Odoos through it.
  duplicateProject: ["projects:write", "servers:write"],
  findOdoo: ["servers:read"],
  listRoutes: ["servers:read", "databases:read"],
  setDatabaseDomain: ["databases:write"],
  listServers: ["servers:read"],
  createServer: ["servers:write"],
  startServer: ["servers:write"],
  stopServer: ["servers:write"],
  listDatabases: ["databases:read"],
  createDatabase: ["databases:write"],
  duplicateDatabase: ["databases:write"],
  dropDatabase: ["databases:write"],
  backupDatabase: ["databases:write"],
  listBackups: ["databases:read"],
  restoreDatabase: ["databases:write"],
  // Changes real rows in a real database, so it is a write — even though
  // what it writes makes things safer.
  neutralizeDatabase: ["databases:write"],
  neutralization: ["databases:read"],
  discoverAddons: ["addons-sources:read"],
  adoptAddonsRoots: ["addons-sources:write"],
  moduleFreshness: ["modules:read"],
  upgradeModulesBatched: ["modules:write"],
  upgradeTargets: ["servers:read"],
  upgradePlan: ["databases:read", "servers:read"],
  // Copies a database, snapshots it repeatedly, and runs real migrations
  // over the copy. Everything it touches is something it made — the
  // original database is never opened for writing.
  runUpgrade: ["databases:write", "snapshots:write"],
  // `system:*`, not `databases:*`. The cache is this app's own storage,
  // the same category as its downloaded copies of Odoo — and
  // `clear_template_cache` can only ever drop names it built itself
  // (`oo_template_*`), which is what makes that classification honest
  // rather than a loophole. The Settings screen deliberately holds no
  // `databases:write`, and this doesn't quietly give it one.
  listTemplates: ["system:read"],
  clearTemplates: ["system:write"],
  createSnapshot: ["snapshots:write"],
  listSnapshots: ["snapshots:read"],
  deleteSnapshot: ["snapshots:write"],
  // Touches both a live database (drop + re-template) and reads/creates
  // snapshot rows — requires both scopes, matching what it actually does
  // server-side (see Core::revert_database_to_snapshot).
  revertDatabaseToSnapshot: ["databases:write", "snapshots:write"],
  getGitBranch: ["git:read"],
  snapshotsOnBranch: ["git:read", "snapshots:read"],
  // Changes how a real Odoo process behaves on its next start (and can
  // install a package into its interpreter), so it needs the same scope as
  // starting or stopping one — not a lesser "settings" scope.
  setReloadPolicy: ["servers:write"],
  previewOdooConf: ["servers:read", "addons-sources:read"],
  recentEvents: ["events:read"],
  listAddonsSources: ["addons-sources:read"],
  createAddonsSource: ["addons-sources:write"],
  scanModules: ["modules:read"],
  installModules: ["modules:write"],
  upgradeModules: ["modules:write"],
  uninstallModules: ["modules:write"],
  getDatabaseModuleStates: ["modules:read"],
  // Asks every running store what it really holds. Reads only — but it
  // reads across both the app's rows and Postgres itself, so it needs
  // both read scopes rather than either alone.
  reconcile: ["databases:read", "postgres:read"],
  // Writes a measured size onto each row. Deliberately *not*
  // `databases:write`: that scope means "can create or destroy a
  // database", and a screen that can only re-measure sizes must not
  // inherit the power to drop one.
  refreshDatabaseSizes: ["databases:read", "postgres:read"],
  adoptDatabase: ["databases:write"],
  // Removes a row for a database that's already gone. Still a write —
  // getting it wrong loses this app's record of a database that exists.
  forgetDatabase: ["databases:write"],
  serverLogs: ["servers:read"],
  diskUsage: ["system:read"],
  processUsage: ["system:read"],
  listOdooCopies: ["system:read"],
  deleteOdooCopy: ["system:write"],
  listPostgresInstances: ["postgres:read"],
  getPostgresInstance: ["postgres:read"],
  createPostgresInstance: ["postgres:write"],
  startPostgresInstance: ["postgres:write"],
  stopPostgresInstance: ["postgres:write"],
};

export type ScopedApi = {
  [K in keyof typeof api]: (typeof api)[K];
};

export function createScopedApi(scopes: Scope[]): ScopedApi {
  const granted = new Set(scopes);
  const scoped = {} as ScopedApi;
  for (const method of Object.keys(SCOPE_REQUIREMENTS) as (keyof typeof api)[]) {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (scoped as any)[method] = guard(SCOPE_REQUIREMENTS[method], method, granted, api[method] as any);
  }
  return scoped;
}

export type ScopedSubscribeEvents = (onEvent: (e: EventEnvelope) => void) => () => void;

export function createScopedSubscribeEvents(scopes: Scope[]): ScopedSubscribeEvents {
  const granted = new Set(scopes);
  return guard<[onEvent: (e: EventEnvelope) => void], () => void>(
    ["events:read"],
    "subscribeEvents",
    granted,
    rawSubscribeEvents,
  );
}
