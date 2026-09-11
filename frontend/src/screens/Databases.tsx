import { useEffect, useMemo, useState } from "react";
import type {
  Database,
  MissingDatabase,
  OdooServer,
  Project,
  Reconciliation,
  Snapshot,
  ClusterDatabase,
} from "../lib/api";
import type { PluginScreenProps } from "../lib/pluginTypes";
import { databaseUrl, formatBytes, plural, proxyPort, timeAgo } from "../lib/format";
import { IconCheck, IconDatabase, IconShield, IconWarn } from "../components/Icons";

/** Every database on this machine, in one table.
 *
 *  Projects answer "what am I working on". This answers "what data is on
 *  this machine", which is a different question and is otherwise
 *  unanswerable without opening every project in turn.
 *
 *  Two things here exist nowhere else in the app:
 *
 *  **Bulk protection.** Selecting eight databases and snapshotting them in
 *  one go is the difference between doing it before something scary and
 *  not doing it at all.
 *
 *  **Reconciliation.** The app's rows and the real cluster drift — someone
 *  runs `createdb` by hand, or drops something in psql. A tool that shows
 *  its own stale opinion while touching client data is worse than no tool,
 *  so both directions of the difference are shown and both are fixable. */

type Filter = "all" | "unprotected" | "running";

interface Row {
  database: Database;
  server: OdooServer | undefined;
  project: Project | undefined;
  snapshots: Snapshot[];
}

export default function Databases({ api, subscribeEvents }: PluginScreenProps) {
  const [projects, setProjects] = useState<Project[]>([]);
  const [servers, setServers] = useState<OdooServer[]>([]);
  const [databases, setDatabases] = useState<Database[]>([]);
  const [snapshots, setSnapshots] = useState<Snapshot[]>([]);
  const [error, setError] = useState<string | null>(null);

  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const [projectFilter, setProjectFilter] = useState<string>("");
  const [selected, setSelected] = useState<Set<string>>(new Set());

  const [reconciliation, setReconciliation] = useState<Reconciliation | null>(null);
  const [reconciling, setReconciling] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [sizesMeasuredAt, setSizesMeasuredAt] = useState<string | null>(null);

  async function refresh() {
    try {
      const [prj, srv, dbs, snaps] = await Promise.all([
        api.listProjects(),
        api.listServers(),
        api.listDatabases(),
        api.listSnapshots(),
      ]);
      setProjects(prj);
      setServers(srv);
      setDatabases(dbs);
      setSnapshots(snaps);
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  useEffect(() => {
    void refresh();
    return subscribeEvents(() => void refresh());
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const rows: Row[] = useMemo(() => {
    const byServer = new Map(servers.map((s) => [s.id, s]));
    const byProject = new Map(projects.map((p) => [p.id, p]));
    const snapsFor = new Map<string, Snapshot[]>();
    for (const snap of snapshots) {
      snapsFor.set(snap.database_id, [...(snapsFor.get(snap.database_id) ?? []), snap]);
    }
    return databases.map((database) => {
      const server = byServer.get(database.server_id);
      return {
        database,
        server,
        project: server?.project_id ? byProject.get(server.project_id) : undefined,
        snapshots: snapsFor.get(database.id) ?? [],
      };
    });
  }, [databases, servers, projects, snapshots]);

  const visible = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return rows.filter((row) => {
      if (needle) {
        const haystack = [row.database.name, row.project?.name ?? "", row.server?.name ?? ""].join(" ").toLowerCase();
        if (!haystack.includes(needle)) return false;
      }
      if (projectFilter && row.project?.id !== projectFilter) return false;
      if (filter === "unprotected" && row.snapshots.length > 0) return false;
      if (filter === "running" && row.server?.state.status !== "running") return false;
      return true;
    });
  }, [rows, query, filter, projectFilter]);

  const port = proxyPort();
  const selectedRows = visible.filter((r) => selected.has(r.database.id));
  const unprotectedCount = rows.filter((r) => r.snapshots.length === 0).length;

  function toggle(id: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  function toggleAll() {
    setSelected((prev) =>
      prev.size === visible.length ? new Set() : new Set(visible.map((r) => r.database.id)),
    );
  }

  // Bulk work runs one at a time and reports what actually happened, per
  // database. Failing halfway through eight snapshots and saying "done" is
  // exactly the kind of lie that makes a safety feature worse than none.
  async function runBulk(kind: "snapshot" | "backup") {
    const targets = selectedRows;
    if (targets.length === 0) return;
    const failures: string[] = [];
    for (const [i, row] of targets.entries()) {
      setBusy(`${kind === "snapshot" ? "Snapshotting" : "Backing up"} ${row.database.name} (${i + 1} of ${targets.length})…`);
      try {
        if (kind === "snapshot") {
          await api.createSnapshot(row.database.id, `Before ${new Date().toLocaleString()}`);
        } else {
          await api.backupDatabase(row.database.id);
        }
      } catch (err) {
        failures.push(`${row.database.name}: ${err instanceof Error ? err.message : String(err)}`);
      }
    }
    setBusy(null);
    setSelected(new Set());
    setError(failures.length > 0 ? `${plural(failures.length, "failed")}. ${failures.join(" · ")}` : null);
    await refresh();
  }

  async function reconcile() {
    setReconciling(true);
    try {
      setReconciliation(await api.reconcile());
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setReconciling(false);
    }
  }

  async function refreshSizes() {
    setBusy("Measuring sizes…");
    try {
      await api.refreshDatabaseSizes();
      setSizesMeasuredAt(new Date().toISOString());
      await refresh();
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(null);
    }
  }

  async function adopt(found: ClusterDatabase) {
    // Adoption needs an Odoo to file it under. The honest default is the
    // one whose store it actually lives in; if several qualify, the first
    // is a starting point the user can move it from afterwards.
    const candidate = servers.find((s) => s.postgres_instance_id === found.instance_id);
    if (!candidate) {
      setError(`Nothing to file "${found.name}" under — make an Odoo on that store first.`);
      return;
    }
    setBusy(`Adopting ${found.name}…`);
    try {
      await api.adoptDatabase(candidate.id, found.name);
      await refresh();
      await reconcile();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(null);
    }
  }

  async function forget(missing: MissingDatabase) {
    setBusy(`Removing the row for ${missing.name}…`);
    try {
      await api.forgetDatabase(missing.database_id);
      await refresh();
      await reconcile();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(null);
    }
  }

  return (
    <div className="scroll" style={{ flex: 1, padding: 26, display: "flex", flexDirection: "column", gap: 14 }}>
      <div>
        <div style={{ fontSize: 22, fontWeight: 700, letterSpacing: "-0.015em" }}>Databases</div>
        <div style={{ fontSize: 13, color: "var(--ink3)", marginTop: 3 }}>
          Every database on this machine — {plural(rows.length, "database")}
          {unprotectedCount > 0 ? `, ${unprotectedCount} with no snapshot to fall back to.` : ", all with a snapshot to fall back to."}
        </div>
      </div>

      {error && (
        <div
          className="card"
          style={{ padding: 13, background: "var(--err-soft)", borderColor: "var(--err)", color: "var(--err)", fontSize: 12.5 }}
        >
          {error}
        </div>
      )}

      <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
        <input
          className="field"
          style={{ width: 240, padding: "8px 11px", fontSize: 12.5 }}
          placeholder="Search databases…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        <div className="segtrack" style={{ padding: 3 }}>
          {(
            [
              ["all", "All"],
              ["unprotected", "No snapshot"],
              ["running", "Running"],
            ] as [Filter, string][]
          ).map(([value, label]) => (
            <button key={value} className={`seg${filter === value ? " on" : ""}`} style={{ padding: "6px 12px" }} onClick={() => setFilter(value)}>
              {label}
            </button>
          ))}
        </div>
        <select
          className="field"
          style={{ width: 170, padding: "8px 11px", fontSize: 12.5, fontFamily: "var(--ui)" }}
          value={projectFilter}
          onChange={(e) => setProjectFilter(e.target.value)}
        >
          <option value="">Every project</option>
          {projects.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </select>
        <div style={{ flex: 1 }} />
        <button className="btn btn-s" onClick={() => void refreshSizes()} disabled={busy !== null}>
          Measure sizes
        </button>
        <button className="btn btn-s" onClick={() => void reconcile()} disabled={reconciling}>
          {reconciling ? "Checking…" : "Check against Postgres"}
        </button>
      </div>

      {sizesMeasuredAt && (
        <div className="m" style={{ fontSize: 11, color: "var(--ink4)", marginTop: -6 }}>
          sizes measured {timeAgo(sizesMeasuredAt)}
        </div>
      )}

      {busy && (
        <div className="card" style={{ padding: 12, fontSize: 12.5, color: "var(--ink2)", display: "flex", alignItems: "center", gap: 9 }}>
          <span className="dot dot-busy" />
          {busy}
        </div>
      )}

      {reconciliation && <ReconcilePanel report={reconciliation} onAdopt={adopt} onForget={forget} />}

      {selectedRows.length > 0 && (
        <div
          className="card"
          style={{
            padding: "11px 14px",
            display: "flex",
            alignItems: "center",
            gap: 12,
            borderColor: "var(--acc-line)",
            background: "var(--acc-soft)",
          }}
        >
          <span style={{ fontSize: 12.5, color: "var(--ink)" }}>{plural(selectedRows.length, "database")} selected</span>
          <div style={{ flex: 1 }} />
          <button className="btn btn-s" onClick={() => void runBulk("backup")} disabled={busy !== null}>
            Back up all {selectedRows.length}
          </button>
          <button className="btn btn-s btn-p" onClick={() => void runBulk("snapshot")} disabled={busy !== null}>
            Snapshot all {selectedRows.length}
          </button>
          <button className="btn btn-s btn-q" onClick={() => setSelected(new Set())}>
            Clear
          </button>
        </div>
      )}

      <div className="card" style={{ overflow: "hidden" }}>
        <div className="drow drow-head">
          <label style={{ display: "flex", alignItems: "center", cursor: "pointer" }}>
            <input
              type="checkbox"
              checked={visible.length > 0 && selected.size === visible.length}
              onChange={toggleAll}
              aria-label="Select every database shown"
            />
          </label>
          <span className="cap">Database</span>
          <span className="cap">Project</span>
          <span className="cap">Odoo</span>
          <span className="cap">Address</span>
          <span className="cap">Size</span>
          <span className="cap">Protection</span>
          <span className="cap">Last backup</span>
        </div>

        {visible.map((row) => {
          const newest = row.snapshots
            .slice()
            .sort((a, b) => b.created_at.localeCompare(a.created_at))[0];
          const running = row.server?.state.status === "running";
          return (
            <div key={row.database.id} className="drow">
              <label style={{ display: "flex", alignItems: "center", cursor: "pointer" }}>
                <input
                  type="checkbox"
                  checked={selected.has(row.database.id)}
                  onChange={() => toggle(row.database.id)}
                  aria-label={`Select ${row.database.name}`}
                />
              </label>

              <span style={{ display: "flex", alignItems: "center", gap: 8, minWidth: 0 }}>
                <span className={`dot ${running ? "dot-run" : "dot-stop"}`} title={running ? "Its Odoo is running" : "Its Odoo is stopped"} />
                <span className="m" style={{ fontSize: 12.5, overflow: "hidden", textOverflow: "ellipsis" }}>
                  {row.database.name}
                </span>
              </span>

              <span style={{ fontSize: 12.5, color: "var(--ink3)", overflow: "hidden", textOverflow: "ellipsis" }}>
                {row.project?.name ?? "—"}
              </span>

              <span style={{ fontSize: 12.5, color: "var(--ink3)", overflow: "hidden", textOverflow: "ellipsis" }}>
                {row.server ? `${row.server.name} · ${row.server.odoo_version}` : "—"}
              </span>

              <span className="m" style={{ fontSize: 11.5, color: "var(--ink3)", overflow: "hidden", textOverflow: "ellipsis" }}>
                {running ? (
                  <a href={`http://${databaseUrl(row.database, port)}`} target="_blank" rel="noreferrer">
                    {databaseUrl(row.database, port)}
                  </a>
                ) : (
                  databaseUrl(row.database, port)
                )}
              </span>

              <span className="m" style={{ fontSize: 11.5, color: "var(--ink3)" }}>
                {row.database.size_bytes > 0 ? formatBytes(row.database.size_bytes) : "—"}
              </span>

              <span style={{ fontSize: 12, display: "flex", alignItems: "center", gap: 6 }}>
                {row.snapshots.length > 0 ? (
                  <>
                    <span style={{ color: "var(--ok)", display: "flex" }}>
                      <IconShield size={13} />
                    </span>
                    <span style={{ color: "var(--ink3)" }}>{timeAgo(newest.created_at)}</span>
                  </>
                ) : (
                  <span style={{ color: "var(--warn)" }}>no snapshot</span>
                )}
              </span>

              <span className="m" style={{ fontSize: 11.5, color: row.database.last_backup_at ? "var(--ink3)" : "var(--ink4)" }}>
                {timeAgo(row.database.last_backup_at)}
              </span>
            </div>
          );
        })}

        {visible.length === 0 && (
          <div style={{ padding: 24, fontSize: 13, color: "var(--ink4)", borderTop: "1px solid var(--hair)" }}>
            {rows.length === 0 ? "No databases yet." : "Nothing matches those filters."}
          </div>
        )}
      </div>
    </div>
  );
}

/** Both directions of the drift between this app's rows and the real
 *  cluster. Deliberately silent when everything agrees — a panel that says
 *  "all good" every time is a panel people stop reading. */
function ReconcilePanel({
  report,
  onAdopt,
  onForget,
}: {
  report: Reconciliation;
  onAdopt: (db: ClusterDatabase) => void;
  onForget: (db: MissingDatabase) => void;
}) {
  const clean = report.untracked.length === 0 && report.missing.length === 0;

  return (
    <div className="card" style={{ padding: 16, display: "flex", flexDirection: "column", gap: 12 }}>
      <div style={{ display: "flex", alignItems: "center", gap: 9 }}>
        <span style={{ color: clean ? "var(--ok)" : "var(--warn)", display: "flex" }}>
          {clean ? <IconCheck size={15} /> : <IconWarn size={15} />}
        </span>
        <span style={{ fontSize: 13, fontWeight: 560 }}>
          {clean ? "This app and Postgres agree" : "This app and Postgres disagree"}
        </span>
      </div>

      {report.unchecked.length > 0 && (
        <div style={{ fontSize: 12.5, color: "var(--ink3)" }}>
          Couldn't ask {report.unchecked.join(", ")} — start {report.unchecked.length === 1 ? "it" : "them"} to include{" "}
          {report.unchecked.length === 1 ? "its" : "their"} databases. Nothing there is being reported as missing.
        </div>
      )}

      {report.untracked.length > 0 && (
        <div>
          <div className="cap" style={{ marginBottom: 6 }}>
            In Postgres, unknown to this app
          </div>
          {report.untracked.map((found) => (
            <div key={`${found.instance_id}:${found.name}`} className="kv">
              <span style={{ display: "flex", alignItems: "center", gap: 8 }}>
                <IconDatabase size={13} />
                <span className="m" style={{ fontSize: 12.5 }}>
                  {found.name}
                </span>
                <span className="m" style={{ fontSize: 11.5, color: "var(--ink4)" }}>
                  {formatBytes(found.size_bytes)}
                </span>
              </span>
              <button className="btn btn-s" onClick={() => onAdopt(found)}>
                Take ownership
              </button>
            </div>
          ))}
        </div>
      )}

      {report.missing.length > 0 && (
        <div>
          <div className="cap" style={{ marginBottom: 6 }}>
            Known to this app, gone from Postgres
          </div>
          {report.missing.map((missing) => (
            <div key={missing.database_id} className="kv">
              <span style={{ display: "flex", alignItems: "center", gap: 8 }}>
                <span className="m" style={{ fontSize: 12.5 }}>
                  {missing.name}
                </span>
                <span style={{ fontSize: 11.5, color: "var(--ink4)" }}>in {missing.store}</span>
              </span>
              <button className="btn btn-s" onClick={() => onForget(missing)} title="Removes this app's row. The database is already gone.">
                Remove the row
              </button>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
