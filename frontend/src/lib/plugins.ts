// Task X.1 — the plugin registry. Per odoo-orchestrator-technical-design.md's
// "Plugin architecture" section: v1 ships the manifest shape and permission
// enforcement (see scopedApi.ts) for real, but implements every actual
// screen as an in-repo "first-party plugin" against that same interface
// rather than building a dynamic loader for third-party code — true
// webview/iframe-isolated third-party plugins are explicitly deferred.
//
// What's real here, not just structural: App.tsx doesn't import screens
// directly or switch on a hardcoded union — it iterates this array for its
// nav rail and hands whichever section is selected a
// `createScopedApi(section.permissions)` it cannot escape (see scopedApi.ts).
// A section that never declares `databases:write` genuinely cannot drop a
// database; the call throws before it reaches the network.
import Workspace from "../screens/Workspace";
import Databases from "../screens/Databases";
import Activity from "../screens/Activity";
import Stats from "../screens/Stats";
import Settings from "../screens/Settings";
import type { PluginManifest } from "./pluginTypes";

export const PLUGINS: PluginManifest[] = [
  {
    id: "workspace",
    navLabel: "Projects",
    // The widest scope set in the app, because this one section holds the
    // whole projects → Odoos → one Odoo drill-down: creating projects and
    // Odoos, making and dropping databases, taking and restoring snapshots,
    // registering module folders, and installing modules. It deliberately
    // does *not* hold `postgres:write` — starting and stopping the storage
    // engine itself belongs to Engine, and this section only needs to read
    // which instance an Odoo uses.
    //
    // `system:read` is here for one narrow thing: telling the truth about
    // how long making a database will take. Without knowing whether a
    // pre-built one already exists, the screen would have to either promise
    // "instant" and sometimes spend a minute booting Odoo, or warn about a
    // minute that usually doesn't happen. `system:write` is deliberately
    // still absent — nothing here deletes the app's cached files.
    permissions: [
      "projects:read",
      "projects:write",
      "servers:read",
      "servers:write",
      "databases:read",
      "databases:write",
      "snapshots:read",
      "snapshots:write",
      "addons-sources:read",
      "addons-sources:write",
      "modules:read",
      "modules:write",
      "postgres:read",
      "git:read",
      "events:read",
      "system:read",
    ],
    component: Workspace,
  },
  {
    id: "databases",
    navLabel: "Databases",
    // Every database on the machine in one table, with bulk protection and
    // reconciliation against the real cluster. Holds `databases:write`
    // because adopting an untracked database and removing the row for a
    // vanished one are both writes — but deliberately not `postgres:write`
    // or `servers:write`: nothing here starts, stops or creates anything.
    permissions: [
      "projects:read",
      "servers:read",
      "databases:read",
      "databases:write",
      "snapshots:read",
      "snapshots:write",
      "postgres:read",
      "events:read",
    ],
    component: Databases,
  },
  {
    id: "activity",
    navLabel: "Activity",
    // Almost read-only. It holds the write scopes for one narrow reason:
    // the inline "undo this" on a row that has a real inverse — restoring
    // a snapshot, undoing a restore, or bringing a dropped database back
    // from its last backup. Nothing else on the screen causes anything to
    // happen.
    permissions: [
      "servers:read",
      "databases:read",
      "databases:write",
      "postgres:read",
      "snapshots:read",
      "snapshots:write",
      "events:read",
    ],
    component: Activity,
  },
  {
    id: "stats",
    navLabel: "Stats",
    // Entirely read-only, including `system:read` for disk usage and the
    // list of downloaded Odoo copies. It can see that three copies are
    // eating 3 GB; deleting one is Settings' job, so a stats page can
    // never free disk by accident.
    permissions: [
      "projects:read",
      "servers:read",
      "databases:read",
      "snapshots:read",
      "modules:read",
      "postgres:read",
      "events:read",
      "system:read",
    ],
    component: Stats,
  },
  {
    id: "settings",
    navLabel: "Settings",
    // The only holder of `system:write` (deleting downloaded copies of
    // Odoo) and, alongside it, `postgres:write` for starting and stopping
    // the database stores. Notably absent: `databases:write` — no setting
    // should ever be able to touch a database.
    permissions: ["postgres:read", "postgres:write", "servers:read", "system:read", "system:write"],
    component: Settings,
  },
];
