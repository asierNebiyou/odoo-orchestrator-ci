// Task X.1's permission-scope taxonomy — the "permissions" half of the
// plugin manifest described in odoo-orchestrator-technical-design.md's
// "Plugin architecture" section ("scoped permissions like 'read-only
// workspace metadata' vs. 'may create/delete databases'").
//
// Deliberately coarse (resource:read / resource:write pairs, not one scope
// per HTTP route) — fine enough that a plugin declaring only *:read scopes
// genuinely cannot mutate anything, coarse enough to stay legible in a
// permission-review UI. Each scope below is a real gate enforced at runtime
// by `createScopedApi` in scopedApi.ts, not just documentation: a plugin
// that calls an api method without the scope it requires gets a thrown
// `ScopeDeniedError`, not a silently-successful call. See that file's
// `SCOPE_REQUIREMENTS` table for exactly which scope(s) each api method
// checks — this file is just the vocabulary.
export type Scope =
  | "projects:read"
  | "projects:write"
  | "servers:read"
  | "servers:write"
  | "databases:read"
  | "databases:write"
  | "postgres:read"
  | "postgres:write"
  | "snapshots:read"
  | "snapshots:write"
  | "addons-sources:read"
  | "addons-sources:write"
  | "modules:read"
  | "modules:write"
  | "git:read"
  | "events:read"
  // The app's own footprint on the machine — disk usage and the cache of
  // downloaded Odoo copies. Separate from `servers:*` on purpose: reading
  // how much disk three copies of Odoo are eating is a different power
  // from creating or starting an Odoo, and deleting one of those copies
  // touches the filesystem rather than any project's data.
  | "system:read"
  | "system:write";

// Shown in a future permission-review UI (not built yet — v1 defers the
// webview/iframe sandboxing this would gate, per the technical-design doc's
// "defer true third-party dynamic/webview loading to a later phase" note).
// Kept here now so that UI has real copy to render on day one instead of
// needing someone to invent it later.
export const SCOPE_DESCRIPTIONS: Record<Scope, string> = {
  "projects:read": "See the list of projects and what each one holds",
  "projects:write": "Create, rename, duplicate, or delete projects",
  "servers:read": "See registered Odoo servers",
  "servers:write": "Create Odoo servers, and start or stop their real odoo-bin process",
  "databases:read": "See databases and their metadata",
  "databases:write": "Create, duplicate, drop, back up, restore, or revert databases",
  "postgres:read": "See Postgres instances and their status",
  "postgres:write": "Create, start, or stop Postgres instances",
  "snapshots:read": "See snapshots",
  "snapshots:write": "Create or delete snapshots",
  "addons-sources:read": "See registered addons sources",
  "addons-sources:write": "Register addons sources",
  "modules:read": "Scan addons sources for modules, and read a database's real installed-module state",
  "modules:write": "Install, upgrade, or uninstall modules on a database",
  "git:read": "Read the current git branch of a linked repo",
  "events:read": "Read the event log, live and historical",
  "system:read": "See how much disk this app is using — downloaded copies of Odoo, and its cache of pre-built databases",
  "system:write": "Delete this app's own cached files — downloaded copies of Odoo, and its pre-built databases. Never anything you made",
};
