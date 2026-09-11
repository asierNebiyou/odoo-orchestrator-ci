import type { EventEnvelope } from "./api";

export interface NameMaps {
  serverNames: Map<string, string>;
  databaseNames: Map<string, string>;
  instanceLabels: Map<string, string>;
  snapshotNames: Map<string, string>;
}

function str(payload: Record<string, unknown>, key: string): string | undefined {
  const v = payload[key];
  return typeof v === "string" ? v : undefined;
}

function num(payload: Record<string, unknown>, key: string): number | undefined {
  const v = payload[key];
  return typeof v === "number" ? v : undefined;
}

function bool(payload: Record<string, unknown>, key: string): boolean {
  return payload[key] === true;
}

function serverName(maps: NameMaps, id: string | undefined): string {
  if (!id) return "a server";
  return maps.serverNames.get(id) ?? `server ${id.slice(0, 8)}`;
}

function databaseName(maps: NameMaps, id: string | undefined): string {
  if (!id) return "a database";
  return maps.databaseNames.get(id) ?? `database ${id.slice(0, 8)}`;
}

function instanceLabel(maps: NameMaps, id: string | undefined): string {
  if (!id) return "a Postgres instance";
  return maps.instanceLabels.get(id) ?? `instance ${id.slice(0, 8)}`;
}

/** Turns one raw event into a short, plain sentence for the Activity log and
 * the Dashboard's recent-activity list. Every case reads straight from the
 * event's payload; a missing field (usually because the row it named was
 * later deleted) falls back to a short id fragment instead of guessing. */
export function describeEvent(e: EventEnvelope, maps: NameMaps): string {
  const p = e.payload;
  switch (e.type) {
    case "project_created":
      return `Created the project "${str(p, "name") ?? "?"}"`;
    case "project_renamed":
      return `Renamed a project to "${str(p, "name") ?? "?"}"`;
    case "project_deleted":
      return "Deleted a project";
    case "project_duplicated": {
      const n = num(p, "server_count") ?? 0;
      return `Copied a project's setup — ${n} Odoo${n === 1 ? "" : "s"}, without their databases`;
    }

    case "server_created":
      return `Created Odoo "${str(p, "name") ?? "?"}" (version ${str(p, "odoo_version") ?? "?"})`;
    case "server_started":
      return `Started "${serverName(maps, str(p, "server_id"))}"`;
    case "server_stopped":
      return `Stopped "${serverName(maps, str(p, "server_id"))}"`;
    case "server_crashed":
      return `"${serverName(maps, str(p, "server_id"))}" stopped unexpectedly (exit ${num(p, "exit_code") ?? "?"})`;

    case "database_created":
      return `Created database "${str(p, "name") ?? databaseName(maps, str(p, "database_id"))}"`;
    case "database_duplicated":
      return `Duplicated "${databaseName(maps, str(p, "source_database_id"))}" into "${databaseName(maps, str(p, "database_id"))}"`;
    // The name comes off the event, not the lookup: by the time anyone
    // reads this row the database is gone from every map, and "Dropped
    // database 3f2a1b0c" is not a sentence anybody can act on.
    case "database_dropped":
      return `Dropped database "${str(p, "name") ?? databaseName(maps, str(p, "database_id"))}"`;
    case "database_backed_up":
      return `Backed up "${databaseName(maps, str(p, "database_id"))}" (${str(p, "format") ?? "dump"})`;
    case "database_restored":
      return `Restored a backup into "${databaseName(maps, str(p, "database_id"))}"`;

    case "modules_scanned": {
      const found = num(p, "module_count") ?? 0;
      const dupes = num(p, "shadow_count") ?? 0;
      return `Checked the apps available to "${serverName(maps, str(p, "server_id"))}": ${found} found${
        dupes > 0 ? `, ${dupes} of them in more than one folder` : ""
      }`;
    }

    case "module_installed":
      return `Installed the app "${str(p, "technical_name") ?? "?"}" on "${databaseName(maps, str(p, "database_id"))}"`;
    case "module_upgraded":
      return `Upgraded "${str(p, "technical_name") ?? "?"}" to ${str(p, "to_version") ?? "?"} on "${databaseName(
        maps,
        str(p, "database_id"),
      )}"`;
    case "module_uninstalled":
      return `Removed the app "${str(p, "technical_name") ?? "?"}" from "${databaseName(maps, str(p, "database_id"))}"`;

    case "postgres_instance_created":
      return `Added database storage "${str(p, "label") ?? "?"}" (PostgreSQL ${str(p, "pg_version") ?? "?"})`;
    case "postgres_starting":
      return `Database storage "${instanceLabel(maps, str(p, "instance_id"))}" starting`;
    case "postgres_running":
      return `Database storage "${instanceLabel(maps, str(p, "instance_id"))}" ready (pid ${num(p, "pid") ?? "?"})`;
    case "postgres_stopping":
      return `Database storage "${instanceLabel(maps, str(p, "instance_id"))}" stopping`;
    case "postgres_stopped":
      return `Database storage "${instanceLabel(maps, str(p, "instance_id"))}" stopped`;
    case "postgres_crashed":
      return `Database storage "${instanceLabel(maps, str(p, "instance_id"))}" stopped unexpectedly${
        num(p, "exit_code") != null ? ` (exit ${num(p, "exit_code")})` : ""
      }`;

    case "snapshot_created":
      return `Snapshotted "${databaseName(maps, str(p, "database_id"))}" as "${str(p, "name") ?? "?"}"${
        bool(p, "has_filestore") ? " (+filestore)" : ""
      }`;
    case "snapshot_deleted":
      return `Deleted snapshot "${maps.snapshotNames.get(str(p, "snapshot_id") ?? "") ?? (str(p, "snapshot_id")?.slice(0, 8) ?? "?")}"`;
    case "database_reverted_to_snapshot":
      return `Reverted "${databaseName(maps, str(p, "database_id"))}" to snapshot "${
        maps.snapshotNames.get(str(p, "snapshot_id") ?? "") ?? (str(p, "snapshot_id")?.slice(0, 8) ?? "?")
      }"${str(p, "counter_snapshot_id") ? " (safety snapshot taken first)" : ""}`;

    case "editor_attached":
      return `Opened the editor on "${databaseName(maps, str(p, "database_id"))}"`;

    default:
      return e.type;
  }
}
