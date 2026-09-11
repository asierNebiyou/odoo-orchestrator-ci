import { useEffect, useMemo, useState } from "react";
import type { Backup, Database, EventEnvelope, OdooServer, PostgresInstance, Snapshot } from "../lib/api";
import type { PluginScreenProps } from "../lib/pluginTypes";
import { describeEvent } from "../lib/eventDescriptions";
import { timeAgo } from "../lib/format";
import { IconUndo } from "../components/Icons";
import { ConfirmDialog, type ConfirmSpec } from "../components/Confirm";

/** A log of things that happened isn't useful on its own. Three questions
 *  are, and this screen is built around them:
 *
 *  **"What happened while I was away?"** — a rule in the timeline at the
 *  point you last had this open. Parnin's resumption research (10-15
 *  minutes to rebuild context after an interruption) is why: this
 *  collapses that into a glance. The timestamp is per viewer, so
 *  localStorage is exactly the right place for it.
 *
 *  **"What touched a client's data?"** — destructive entries get their own
 *  visual lane, because that's what people actually scan a log for.
 *
 *  **"Can I undo that?"** — where a *real* inverse exists, the row offers
 *  it. This is the reversibility thesis made concrete: the safety net is
 *  visible at the moment you're looking at the thing that scared you.
 *  Rows with no real inverse offer nothing and say so by having no button
 *  — never a button that only appears to undo something. */

const LAST_SEEN_KEY = "orchestrator:activityLastSeen";
const HEATMAP_DAYS = 12 * 7;

type Lane = "all" | "databases" | "modules" | "snapshots" | "destructive";

/** Events that destroy or overwrite data. Everything here is in the
 *  destructive lane; nothing else is. */
const DESTRUCTIVE = new Set([
  "database_dropped",
  "database_reverted_to_snapshot",
  "snapshot_deleted",
  "project_deleted",
  "module_uninstalled",
  "server_crashed",
  "postgres_crashed",
]);

const LANES: Record<Exclude<Lane, "all" | "destructive">, string[]> = {
  databases: ["database_created", "database_duplicated", "database_dropped", "database_backed_up", "database_restored"],
  modules: ["modules_scanned", "module_installed", "module_upgraded", "module_uninstalled"],
  snapshots: ["snapshot_created", "snapshot_deleted", "database_reverted_to_snapshot"],
};

function readLastSeen(): number {
  try {
    const raw = localStorage.getItem(LAST_SEEN_KEY);
    const n = raw ? Number(raw) : NaN;
    return Number.isFinite(n) ? n : 0;
  } catch {
    return 0;
  }
}

export default function Activity({ api, subscribeEvents }: PluginScreenProps) {
  const [servers, setServers] = useState<OdooServer[]>([]);
  const [databases, setDatabases] = useState<Database[]>([]);
  const [instances, setInstances] = useState<PostgresInstance[]>([]);
  const [snapshots, setSnapshots] = useState<Snapshot[]>([]);
  const [events, setEvents] = useState<EventEnvelope[]>([]);
  const [backups, setBackups] = useState<Backup[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [lane, setLane] = useState<Lane>("all");
  const [confirm, setConfirm] = useState<ConfirmSpec | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  // Captured once on mount, then written forward. Reading it into state
  // first is what makes the marker survive the write that immediately
  // follows — otherwise looking at the screen would erase the very line
  // it exists to draw.
  const [lastSeen] = useState<number>(readLastSeen);

  async function refresh() {
    try {
      const [srv, dbs, insts, snaps, evts, bkps] = await Promise.all([
        api.listServers(),
        api.listDatabases(),
        api.listPostgresInstances(),
        api.listSnapshots(),
        api.recentEvents(300),
        // Read so a backup row can offer a restore only when the dump is
        // genuinely still on disk. A person may have moved or deleted it
        // outside this app, and offering a button that then fails is the
        // thing this screen's whole inverse rule exists to prevent.
        api.listBackups(),
      ]);
      setServers(srv);
      setDatabases(dbs);
      setInstances(insts);
      setSnapshots(snaps);
      setEvents(evts);
      setBackups(bkps);
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  useEffect(() => {
    void refresh();
    const unsubscribe = subscribeEvents(() => void refresh());
    return () => {
      unsubscribe();
      try {
        localStorage.setItem(LAST_SEEN_KEY, String(Date.now()));
      } catch {
        // The marker is a convenience; the log is unaffected.
      }
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const maps = useMemo(
    () => ({
      serverNames: new Map(servers.map((s) => [s.id, s.name])),
      databaseNames: new Map(databases.map((d) => [d.id, d.name])),
      instanceLabels: new Map(instances.map((i) => [i.id, i.label])),
      snapshotNames: new Map(snapshots.map((s) => [s.id, s.name])),
    }),
    [servers, databases, instances, snapshots],
  );

  const snapshotsById = useMemo(() => new Map(snapshots.map((s) => [s.id, s])), [snapshots]);
  const backupsByPath = useMemo(() => new Map(backups.map((b) => [b.path, b])), [backups]);
  /** The newest surviving backup of each database, for the "it was dropped,
   *  can I get it back" case. */
  const newestBackupFor = useMemo(() => {
    const byDatabase = new Map<string, Backup>();
    // `listBackups` returns newest first, so the first one wins.
    for (const backup of backups) {
      if (backup.database_id && !byDatabase.has(backup.database_id)) byDatabase.set(backup.database_id, backup);
    }
    return byDatabase;
  }, [backups]);

  const filtered = useMemo(() => {
    if (lane === "all") return events;
    if (lane === "destructive") return events.filter((e) => DESTRUCTIVE.has(e.type));
    return events.filter((e) => LANES[lane].includes(e.type));
  }, [events, lane]);

  const days = useMemo(() => groupByDay(filtered), [filtered]);
  const dayCounts = useMemo(() => bucketEventsByDay(events, HEATMAP_DAYS), [events]);
  const sinceLastSeen = events.filter((e) => new Date(e.occurred_at).getTime() > lastSeen).length;

  /** The inverse of an event, when a real one exists.
   *
   *  Deliberately conservative. "Undo this restore" is offered only when
   *  the revert actually recorded a counter-snapshot, and "restore it" only
   *  when the snapshot row is still there — an offer that fails when
   *  pressed is worse than no offer at all. */
  function inverseFor(e: EventEnvelope): { label: string; run: () => void } | null {
    const p = e.payload;
    const databaseId = typeof p.database_id === "string" ? p.database_id : null;

    if (e.type === "database_reverted_to_snapshot" && databaseId) {
      const counterId = typeof p.counter_snapshot_id === "string" ? p.counter_snapshot_id : null;
      const counter = counterId ? snapshotsById.get(counterId) : undefined;
      if (!counter) return null;
      const name = maps.databaseNames.get(databaseId) ?? "this database";
      return {
        label: "Undo this restore",
        run: () =>
          setConfirm({
            rung: 3,
            title: "Undo this restore",
            object: name,
            consequence: `Overwrites ${name} with "${counter.name}" — the safety snapshot taken just before the restore.`,
            reversibility: "Reversible: another safety snapshot is taken first, unless you turn it off below.",
            actionLabel: `Undo, restoring "${counter.name}"`,
            offerCounterSnapshot: true,
            onConfirm: ({ takeCounterSnapshot }) =>
              void run(`Undoing the restore of ${name}…`, () =>
                api.revertDatabaseToSnapshot(
                  databaseId,
                  counter.id,
                  takeCounterSnapshot ? `Before undo ${new Date().toLocaleString()}` : undefined,
                ),
              ),
          }),
      };
    }

    if (e.type === "snapshot_created" && databaseId) {
      const snapshotId = typeof p.snapshot_id === "string" ? p.snapshot_id : null;
      const snapshot = snapshotId ? snapshotsById.get(snapshotId) : undefined;
      if (!snapshot) return null;
      const name = maps.databaseNames.get(databaseId) ?? "this database";
      return {
        label: "Restore it",
        run: () =>
          setConfirm({
            rung: 3,
            title: `Restore "${snapshot.name}"`,
            object: name,
            consequence: `Replaces everything in ${name} with the contents of "${snapshot.name}", taken ${timeAgo(
              snapshot.created_at,
            )}.`,
            reversibility: "Reversible: a safety snapshot of the current state is taken first.",
            actionLabel: `Restore ${name}`,
            offerCounterSnapshot: true,
            onConfirm: ({ takeCounterSnapshot }) =>
              void run(`Restoring ${name}…`, () =>
                api.revertDatabaseToSnapshot(
                  databaseId,
                  snapshot.id,
                  takeCounterSnapshot ? `Before restore ${new Date().toLocaleString()}` : undefined,
                ),
              ),
          }),
      };
    }

    // Both of these restore a dump, which always lands in a *new*
    // database — so the inverse of "backed up" is not "un-back-up", it is
    // "put this copy somewhere I can look at it", and the inverse of
    // "dropped" is "bring it back under its own name, which is now free".
    const serverId = typeof p.server_id === "string" ? p.server_id : null;

    if (e.type === "database_backed_up" && serverId) {
      const path = typeof p.path === "string" ? p.path : null;
      // Only when the file is genuinely still there.
      const backup = path ? backupsByPath.get(path) : undefined;
      if (!backup) return null;
      const name = maps.databaseNames.get(databaseId ?? "") ?? backup.database_name;
      const into = freeName(`${backup.database_name}_restored`);
      return {
        label: "Restore this backup",
        run: () =>
          setConfirm({
            // Rung 2: nothing existing is touched. Restoring a dump only
            // ever adds a database, so the ladder's higher rungs would be
            // theatre.
            rung: 2,
            title: "Restore this backup",
            object: into,
            consequence: `Creates a new database "${into}" from the backup of ${name} taken ${timeAgo(backup.taken_at)}, neutralized so it can't send mail or run scheduled jobs.`,
            reversibility: `Nothing existing is touched — ${name} and everything else stays exactly as it is.`,
            actionLabel: `Restore into "${into}"`,
            onConfirm: () =>
              void run(`Restoring into ${into}…`, () => api.restoreDatabase(serverId, into, backup.path)),
          }),
      };
    }

    if (e.type === "database_dropped" && serverId && databaseId) {
      const backup = newestBackupFor.get(databaseId);
      if (!backup) return null;
      const droppedName = typeof p.name === "string" ? p.name : backup.database_name;
      const into = freeName(droppedName);
      return {
        label: "Restore from its last backup",
        run: () =>
          setConfirm({
            rung: 2,
            title: `Bring "${droppedName}" back`,
            object: into,
            consequence: `Creates "${into}" from the backup taken ${timeAgo(backup.taken_at)}${
              backup.has_filestore ? ", attachments included" : " — rows only, the attachments weren't in this backup"
            }, neutralized so it can't send mail or run scheduled jobs.`,
            reversibility: "Nothing existing is touched. Anything done after that backup was taken isn't in it.",
            actionLabel: `Restore into "${into}"`,
            onConfirm: () =>
              void run(`Restoring ${droppedName}…`, () => api.restoreDatabase(serverId, into, backup.path)),
          }),
      };
    }

    return null;
  }

  /** A database name nothing is using. Restoring never overwrites, so the
   *  only way this could go wrong is a collision — and the fix is to pick
   *  a different name, not to ask the user to. */
  function freeName(preferred: string): string {
    const taken = new Set(databases.map((d) => d.name));
    if (!taken.has(preferred)) return preferred;
    for (let n = 2; ; n += 1) {
      const candidate = `${preferred}_${n}`;
      if (!taken.has(candidate)) return candidate;
    }
  }

  async function run(label: string, work: () => Promise<unknown>) {
    setConfirm(null);
    setBusy(label);
    try {
      await work();
      await refresh();
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(null);
    }
  }

  return (
    <div className="scroll" style={{ flex: 1, padding: 26, display: "flex", flexDirection: "column", gap: 14 }}>
      <div>
        <div style={{ fontSize: 22, fontWeight: 700, letterSpacing: "-0.015em" }}>Activity</div>
        <div style={{ fontSize: 13, color: "var(--ink3)", marginTop: 3 }}>
          {sinceLastSeen > 0
            ? `${sinceLastSeen} thing${sinceLastSeen === 1 ? "" : "s"} happened since you last had this open.`
            : "Everything that's happened, newest first."}
        </div>
      </div>

      {error && (
        <div className="card" style={{ padding: 13, background: "var(--err-soft)", borderColor: "var(--err)", color: "var(--err)", fontSize: 12.5 }}>
          {error}
        </div>
      )}

      <div className="segtrack" style={{ padding: 3, alignSelf: "flex-start" }}>
        {(
          [
            ["all", "All"],
            ["databases", "Databases"],
            ["modules", "Modules"],
            ["snapshots", "Snapshots"],
            ["destructive", "Destructive"],
          ] as [Lane, string][]
        ).map(([value, label]) => (
          <button
            key={value}
            className={`seg${lane === value ? " on" : ""}`}
            style={{ padding: "6px 14px", color: value === "destructive" && lane !== value ? "var(--err)" : undefined }}
            onClick={() => setLane(value)}
          >
            {label}
          </button>
        ))}
      </div>

      {busy && (
        <div className="card" style={{ padding: 12, fontSize: 12.5, display: "flex", alignItems: "center", gap: 9 }}>
          <span className="dot dot-busy" />
          {busy}
        </div>
      )}

      <div className="card" style={{ padding: 18 }}>
        <div className="cap" style={{ marginBottom: 12 }}>
          Last {HEATMAP_DAYS / 7} weeks
        </div>
        <Heatmap dayCounts={dayCounts} />
      </div>

      {days.map((day) => (
        <div key={day.key}>
          <div className="cap" style={{ margin: "6px 0 8px 2px" }}>
            {day.label}
          </div>
          <div className="card" style={{ overflow: "hidden" }}>
            {day.entries.map((entry, i) => {
              const first = entry.events[0];
              const destructive = DESTRUCTIVE.has(first.type);
              const inverse = entry.events.length === 1 ? inverseFor(first) : null;
              const crossesMarker =
                lastSeen > 0 &&
                new Date(first.occurred_at).getTime() > lastSeen &&
                (i + 1 >= day.entries.length ||
                  new Date(day.entries[i + 1].events[0].occurred_at).getTime() <= lastSeen);

              return (
                <div key={first.id}>
                  <div
                    className={destructive ? "ev-danger" : undefined}
                    style={{
                      borderTop: i === 0 ? "none" : "1px solid var(--hair)",
                      padding: "10px 16px",
                      display: "flex",
                      gap: 12,
                      alignItems: "center",
                    }}
                  >
                    <span className="m" style={{ fontSize: 11.5, color: "var(--ink4)", width: 62, flexShrink: 0 }}>
                      {new Date(first.occurred_at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}
                    </span>
                    <span style={{ fontSize: 12.5, color: destructive ? "var(--ink)" : "var(--ink2)", flex: 1, minWidth: 0 }}>
                      {describeEvent(first, maps)}
                      {entry.events.length > 1 && (
                        <span className="m" style={{ color: "var(--ink4)", marginLeft: 8, fontSize: 11.5 }}>
                          ×{entry.events.length}
                        </span>
                      )}
                    </span>
                    {typeof first.duration_ms === "number" && (
                      <span className="m" style={{ fontSize: 11, color: "var(--ink4)", flexShrink: 0 }}>
                        {first.duration_ms < 1000 ? `${first.duration_ms}ms` : `${(first.duration_ms / 1000).toFixed(1)}s`}
                      </span>
                    )}
                    {inverse && (
                      <button className="btn btn-s btn-q" onClick={inverse.run} style={{ flexShrink: 0, color: "var(--acc-ink)" }}>
                        <IconUndo size={12} />
                        {inverse.label}
                      </button>
                    )}
                  </div>

                  {crossesMarker && (
                    <div
                      style={{
                        display: "flex",
                        alignItems: "center",
                        gap: 10,
                        padding: "0 16px",
                        height: 22,
                        borderTop: "1px solid var(--acc-line)",
                        background: "var(--acc-soft)",
                      }}
                    >
                      <span className="cap" style={{ color: "var(--acc-ink)" }}>
                        you were last here
                      </span>
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        </div>
      ))}

      {days.length === 0 && (
        <div className="card" style={{ padding: 24, fontSize: 13, color: "var(--ink4)" }}>
          {events.length === 0 ? "Nothing has happened yet." : "Nothing in this filter."}
        </div>
      )}

      {confirm && <ConfirmDialog spec={confirm} onClose={() => setConfirm(null)} />}
    </div>
  );
}

// --- grouping ------------------------------------------------------------

interface Entry {
  /** Consecutive identical entries collapse into one row with a count. */
  events: EventEnvelope[];
}

interface Day {
  key: string;
  label: string;
  entries: Entry[];
}

function groupByDay(events: EventEnvelope[]): Day[] {
  const byDay = new Map<string, EventEnvelope[]>();
  for (const e of events) {
    const key = localDateKey(new Date(e.occurred_at));
    byDay.set(key, [...(byDay.get(key) ?? []), e]);
  }
  return [...byDay.entries()]
    .sort((a, b) => b[0].localeCompare(a[0]))
    .map(([key, list]) => ({ key, label: dayLabel(key), entries: collapse(list) }));
}

/** Runs of the same event kind on the same object collapse to one row.
 *  Twelve "modules scanned" in a row is one fact, not twelve. */
function collapse(events: EventEnvelope[]): Entry[] {
  const out: Entry[] = [];
  for (const e of events) {
    const previous = out[out.length - 1];
    if (previous && signature(previous.events[0]) === signature(e)) {
      previous.events.push(e);
    } else {
      out.push({ events: [e] });
    }
  }
  return out;
}

function signature(e: EventEnvelope): string {
  const p = e.payload;
  const subject = ["database_id", "server_id", "instance_id", "project_id"]
    .map((k) => (typeof p[k] === "string" ? p[k] : ""))
    .join("|");
  return `${e.type}:${subject}`;
}

function dayLabel(key: string): string {
  const today = localDateKey(new Date());
  if (key === today) return "Today";
  const yesterday = new Date();
  yesterday.setDate(yesterday.getDate() - 1);
  if (key === localDateKey(yesterday)) return "Yesterday";
  return new Date(`${key}T00:00:00`).toLocaleDateString([], { weekday: "short", day: "numeric", month: "short" });
}

// --- heatmap -------------------------------------------------------------

interface DayCount {
  date: string; // yyyy-mm-dd, local
  count: number;
}

function bucketEventsByDay(events: EventEnvelope[], days: number): DayCount[] {
  const counts = new Map<string, number>();
  for (const e of events) {
    const key = localDateKey(new Date(e.occurred_at));
    counts.set(key, (counts.get(key) ?? 0) + 1);
  }
  const today = new Date();
  const out: DayCount[] = [];
  for (let i = days - 1; i >= 0; i--) {
    const d = new Date(today);
    d.setDate(d.getDate() - i);
    const key = localDateKey(d);
    out.push({ date: key, count: counts.get(key) ?? 0 });
  }
  return out;
}

function localDateKey(d: Date): string {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

function intensity(count: number): string {
  if (count <= 0) return "var(--s2)";
  if (count <= 2) return "var(--acc-soft)";
  if (count <= 5) return "var(--acc)";
  return "var(--acc-ink)";
}

function Heatmap({ dayCounts }: { dayCounts: DayCount[] }) {
  // Padded to whole weeks starting on a Sunday — the layout of a
  // contribution graph, deliberately without its streak logic.
  const first = dayCounts[0];
  const firstDow = first ? new Date(`${first.date}T00:00:00`).getDay() : 0;
  const padded: (DayCount | null)[] = [...Array(firstDow).fill(null), ...dayCounts];
  const weeks: (DayCount | null)[][] = [];
  for (let i = 0; i < padded.length; i += 7) {
    weeks.push(padded.slice(i, i + 7));
  }

  return (
    <div style={{ display: "flex", gap: 3, alignItems: "flex-start", flexWrap: "wrap" }}>
      <div style={{ display: "flex", gap: 3 }}>
        {weeks.map((week, wi) => (
          <div key={wi} style={{ display: "flex", flexDirection: "column", gap: 3 }}>
            {week.map((day, di) =>
              day ? (
                <div
                  key={di}
                  title={`${day.date}: ${day.count} event${day.count === 1 ? "" : "s"}`}
                  style={{ width: 11, height: 11, borderRadius: 2, background: intensity(day.count) }}
                />
              ) : (
                <div key={di} style={{ width: 11, height: 11 }} />
              ),
            )}
          </div>
        ))}
      </div>
      <div style={{ display: "flex", alignItems: "center", gap: 4, marginLeft: 10, fontSize: 11.5, color: "var(--ink3)" }}>
        less
        <div style={{ width: 11, height: 11, borderRadius: 2, background: intensity(0) }} />
        <div style={{ width: 11, height: 11, borderRadius: 2, background: intensity(1) }} />
        <div style={{ width: 11, height: 11, borderRadius: 2, background: intensity(3) }} />
        <div style={{ width: 11, height: 11, borderRadius: 2, background: intensity(6) }} />
        more
      </div>
    </div>
  );
}
