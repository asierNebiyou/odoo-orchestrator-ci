import { useCallback, useEffect, useMemo, useState } from "react";
import type { Database, OdooServer, Project, Snapshot } from "../lib/api";
import type { ScopedApi, ScopedSubscribeEvents } from "../lib/scopedApi";
import { ConfirmDialog, type ConfirmSpec } from "../components/Confirm";
import { NewOdooWizard, describeError } from "../components/NewOdooWizard";
import { Status, isBusy } from "../components/Status";
import {
  IconChevronDown,
  IconChevronLeft,
  IconChevronRight,
  IconDots,
  IconExternal,
  IconInfo,
  IconPlus,
  IconStack,
} from "../components/Icons";
import { Banner } from "./Projects";
import { databaseUrl, formatBytes, plural, proxyPort, timeAgo, uptime } from "../lib/format";

/** Inside a project: its Odoos as expandable rows, each holding its own
 *  databases.
 *
 *  These are rows, not cards, on purpose — the design doc's Jakob's-law
 *  point is that an instance list should borrow the `docker ps` mental
 *  model, and that monospaced tabular data with a status column is what
 *  signals the tool was built by someone who has run it. The accordion is
 *  progressive disclosure *per object*, not a global density toggle: a
 *  senior dev leaves the one problematic Odoo expanded and the rest shut. */

interface Props {
  api: ScopedApi;
  subscribeEvents: ScopedSubscribeEvents;
  projectId: string;
  onBack: () => void;
  onOpenInstance: (serverId: string) => void;
}

export function ProjectDetail({ api, subscribeEvents, projectId, onBack, onOpenInstance }: Props) {
  const [project, setProject] = useState<Project | null>(null);
  const [servers, setServers] = useState<OdooServer[]>([]);
  const [databases, setDatabases] = useState<Database[]>([]);
  const [snapshots, setSnapshots] = useState<Snapshot[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [confirm, setConfirm] = useState<ConfirmSpec | null>(null);
  const [wizard, setWizard] = useState(false);
  const [addingDbTo, setAddingDbTo] = useState<string | null>(null);
  const [newDbName, setNewDbName] = useState("");
  const [pending, setPending] = useState<Record<string, string>>({});

  const reload = useCallback(async () => {
    try {
      const [projects, s, d, snaps] = await Promise.all([
        api.listProjects(),
        api.listServers(),
        api.listDatabases(),
        api.listSnapshots(),
      ]);
      setProject(projects.find((p) => p.id === projectId) ?? null);
      const mine = s.filter((x) => x.project_id === projectId);
      setServers(mine);
      const ids = new Set(mine.map((x) => x.id));
      setDatabases(d.filter((x) => ids.has(x.server_id)));
      setSnapshots(snaps);
      setLoaded(true);
      // Open everything the first time; after that respect what the user
      // has collapsed rather than re-expanding under them on every event.
      setExpanded((prev) => (prev.size === 0 && mine.length > 0 ? new Set(mine.map((x) => x.id)) : prev));
    } catch (err) {
      setError(String(err));
      setLoaded(true);
    }
  }, [api, projectId]);

  useEffect(() => {
    void reload();
  }, [reload]);

  // Live push rather than polling — never block the UI, reconcile in the
  // background when the core says something actually changed.
  useEffect(() => subscribeEvents(() => void reload()), [subscribeEvents, reload]);

  const protectedDbs = useMemo(() => new Set(snapshots.map((s) => s.database_id)), [snapshots]);

  function mark(id: string, label: string | null) {
    setPending((p) => {
      const next = { ...p };
      if (label === null) delete next[id];
      else next[id] = label;
      return next;
    });
  }

  async function startOdoo(server: OdooServer) {
    mark(server.id, "starting");
    try {
      // The first start of a version fetches it, so this can take minutes.
      await api.startServer(server.id);
    } catch (err) {
      setError(describeError(err));
    } finally {
      mark(server.id, null);
      void reload();
    }
  }

  async function stopOdoo(server: OdooServer) {
    // Rung 0 on the friction ladder: immediate, no dialog. Stopping is
    // trivially reversible and gets done constantly; a confirm here is
    // exactly the habituation trainer the design doc warns about.
    mark(server.id, "stopping");
    try {
      await api.stopServer(server.id);
    } catch (err) {
      setError(describeError(err));
    } finally {
      mark(server.id, null);
      void reload();
    }
  }

  async function addDatabase(server: OdooServer) {
    const name = newDbName.trim();
    if (!name) return;
    mark(server.id, "creating database");
    try {
      await api.createDatabase(server.id, name);
      setNewDbName("");
      setAddingDbTo(null);
    } catch (err) {
      setError(describeError(err));
    } finally {
      mark(server.id, null);
      void reload();
    }
  }

  async function backup(db: Database) {
    mark(db.id, "backing up");
    try {
      await api.backupDatabase(db.id);
    } catch (err) {
      setError(describeError(err));
    } finally {
      mark(db.id, null);
      void reload();
    }
  }

  function askDropDatabase(db: Database, server: OdooServer) {
    const hasSnapshot = protectedDbs.has(db.id);
    setConfirm({
      // Rung 4: dropping a database destroys real data on a real cluster.
      rung: 4,
      title: "Drop this database?",
      object: db.name,
      consequence: `${formatBytes(db.size_bytes)} on ${server.name}, dropped from Postgres for good`,
      reversibility: hasSnapshot
        ? "You have a snapshot of this database, so its data can be brought back — but this row and its address go away now."
        : "There is no snapshot of this database. Once it's dropped, its data is gone.",
      ripple: "Anything pointed at its address stops working immediately.",
      actionLabel: `Drop ${db.name}`,
      onConfirm: async () => {
        setConfirm(null);
        mark(db.id, "dropping");
        try {
          await api.dropDatabase(db.id);
        } catch (err) {
          setError(describeError(err));
        } finally {
          mark(db.id, null);
          void reload();
        }
      },
    });
  }

  const totalDbs = databases.length;

  return (
    <div style={{ flex: 1, display: "flex", flexDirection: "column", minWidth: 0 }}>
      <div
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          padding: "20px 26px 14px",
          flexShrink: 0,
          gap: 12,
        }}
      >
        <div style={{ display: "flex", alignItems: "center", gap: 12, minWidth: 0 }}>
          <button className="btn btn-q btn-icon" aria-label="Back to projects" onClick={onBack}>
            <IconChevronLeft />
          </button>
          <span style={{ fontSize: 17, fontWeight: 600, letterSpacing: "-0.018em" }}>
            {project?.name ?? "Project"}
          </span>
          <span style={{ fontSize: 12.5, color: "var(--ink4)" }}>
            {plural(servers.length, "Odoo")} · {plural(totalDbs, "database")}
          </span>
        </div>
        <button className="btn btn-p" onClick={() => setWizard(true)}>
          <IconPlus />
          New Odoo
        </button>
      </div>

      {error ? (
        <Banner tone="err" onDismiss={() => setError(null)}>
          {error}
        </Banner>
      ) : null}

      {servers.length > 1 ? (
        <div
          style={{
            margin: "0 26px 16px",
            display: "flex",
            alignItems: "flex-start",
            gap: 11,
            background: "var(--s1)",
            border: "1px solid var(--hair)",
            borderRadius: 9,
            padding: "12px 15px",
          }}
        >
          <span style={{ color: "var(--ink4)", display: "flex", marginTop: 1 }}>
            <IconInfo />
          </span>
          <div style={{ fontSize: 12.5, color: "var(--ink3)", lineHeight: 1.6 }}>
            Databases under the same Odoo share its version and its modules — cheap, and they start together.{" "}
            <span style={{ color: "var(--ink2)" }}>Need a different version or different modules? That's its own Odoo.</span>
          </div>
        </div>
      ) : null}

      <div
        className="scroll"
        style={{ flex: 1, padding: "0 26px 26px", display: "flex", flexDirection: "column", gap: 16 }}
      >
        {!loaded ? (
          <div className="card" style={{ height: 128, opacity: 0.5 }} />
        ) : servers.length === 0 ? (
          <div
            className="card"
            style={{
              padding: "40px 32px",
              display: "flex",
              flexDirection: "column",
              alignItems: "center",
              textAlign: "center",
              gap: 8,
            }}
          >
            <div style={{ fontSize: 17, fontWeight: 600, letterSpacing: "-0.018em" }}>Nothing here yet.</div>
            <div style={{ fontSize: 12.5, color: "var(--ink3)", maxWidth: 400, lineHeight: 1.6 }}>
              Add an Odoo to this project — name it, pick a version, and you'll have a database you can log into.
            </div>
            <button className="btn btn-p" style={{ marginTop: 10 }} onClick={() => setWizard(true)}>
              <IconPlus />
              New Odoo
            </button>
          </div>
        ) : (
          servers.map((server) => {
            const dbs = databases.filter((d) => d.server_id === server.id);
            const open = expanded.has(server.id);
            const busyLabel = pending[server.id];
            return (
              <div key={server.id} className="card" style={{ overflow: "hidden" }}>
                <div
                  style={{
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "space-between",
                    padding: "15px 16px",
                    gap: 12,
                  }}
                >
                  <div style={{ display: "flex", alignItems: "center", gap: 13, minWidth: 0, flex: 1 }}>
                    <button
                      className="btn btn-q btn-icon"
                      aria-label={open ? `Collapse ${server.name}` : `Expand ${server.name}`}
                      aria-expanded={open}
                      onClick={() =>
                        setExpanded((prev) => {
                          const next = new Set(prev);
                          if (next.has(server.id)) next.delete(server.id);
                          else next.add(server.id);
                          return next;
                        })
                      }
                      style={{ color: "var(--ink4)" }}
                    >
                      {open ? <IconChevronDown /> : <IconChevronRight size={13} />}
                    </button>
                    <div className="tile">
                      <IconStack size={17} strokeWidth={1.8} />
                    </div>
                    <button
                      onClick={() => onOpenInstance(server.id)}
                      style={{
                        display: "flex",
                        flexDirection: "column",
                        gap: 2,
                        background: "transparent",
                        border: "none",
                        padding: 0,
                        cursor: "pointer",
                        textAlign: "left",
                        minWidth: 0,
                        font: "inherit",
                        color: "inherit",
                      }}
                    >
                      <span style={{ display: "flex", alignItems: "center", gap: 10, minWidth: 0 }}>
                        <span
                          style={{
                            fontSize: 14,
                            fontWeight: 500,
                            letterSpacing: "-0.013em",
                            whiteSpace: "nowrap",
                            overflow: "hidden",
                            textOverflow: "ellipsis",
                          }}
                        >
                          {server.name}
                        </span>
                        <span className="badge">
                          ODOO {server.odoo_version} · {dbs.length} DB{dbs.length === 1 ? "" : "S"}
                        </span>
                      </span>
                      <span className="m" style={{ fontSize: 11, color: "var(--ink4)" }}>
                        port {server.port}
                        {server.state.status === "running" && server.started_at
                          ? ` · up ${uptime(server.started_at)}`
                          : ""}
                      </span>
                    </button>
                  </div>
                  <div style={{ display: "flex", alignItems: "center", gap: 11, flexShrink: 0 }}>
                    {busyLabel ? (
                      <span
                        style={{ display: "inline-flex", alignItems: "center", gap: 7, fontSize: 11.5, color: "var(--ink3)" }}
                      >
                        <span className="dot dot-busy" />
                        <span className="m">{busyLabel}</span>
                      </span>
                    ) : (
                      <Status state={server.state} />
                    )}
                    {server.state.status === "running" ? (
                      <button className="btn btn-s" disabled={!!busyLabel} onClick={() => void stopOdoo(server)}>
                        Stop
                      </button>
                    ) : (
                      <button
                        className="btn btn-s"
                        disabled={!!busyLabel || isBusy(server.state)}
                        onClick={() => void startOdoo(server)}
                      >
                        Start
                      </button>
                    )}
                    <button
                      className="btn btn-q btn-icon"
                      aria-label={`Open ${server.name}`}
                      onClick={() => onOpenInstance(server.id)}
                    >
                      <IconDots />
                    </button>
                  </div>
                </div>

                {open ? (
                  <>
                    {dbs.map((db) => {
                      const dbBusy = pending[db.id];
                      return (
                        <div className="db db-wide" key={db.id}>
                          <div style={{ display: "flex", alignItems: "center", gap: 11, minWidth: 0 }}>
                            <span
                              className={`dot ${server.state.status === "running" ? "dot-run" : "dot-stop"}`}
                            />
                            <span className="m" style={{ fontSize: 12.5, color: "var(--ink)" }}>
                              {db.name}
                            </span>
                            {!protectedDbs.has(db.id) ? (
                              <span style={{ fontSize: 11, color: "var(--warn)" }}>no snapshot</span>
                            ) : (
                              <span style={{ fontSize: 11, color: "var(--ink4)" }}>
                                backed up {timeAgo(db.last_backup_at)}
                              </span>
                            )}
                          </div>
                          <span className="m" style={{ fontSize: 11.5, color: "var(--ink3)" }}>
                            {databaseUrl(db, proxyPort())}
                          </span>
                          <span className="m" style={{ fontSize: 11.5, color: "var(--ink4)" }}>
                            {formatBytes(db.size_bytes)}
                          </span>
                          <div style={{ display: "flex", gap: 6, justifyContent: "flex-end" }}>
                            {dbBusy ? (
                              <span
                                style={{
                                  display: "inline-flex",
                                  alignItems: "center",
                                  gap: 6,
                                  fontSize: 11,
                                  color: "var(--ink3)",
                                }}
                              >
                                <span className="dot dot-busy" />
                                <span className="m">{dbBusy}</span>
                              </span>
                            ) : (
                              <>
                                <button className="btn btn-s btn-q" onClick={() => void backup(db)}>
                                  Back up
                                </button>
                                <button
                                  className="btn btn-s btn-q"
                                  style={{ color: "var(--ink4)" }}
                                  onClick={() => askDropDatabase(db, server)}
                                >
                                  Drop
                                </button>
                                <a
                                  className="btn btn-s"
                                  href={`http://${databaseUrl(db, proxyPort())}`}
                                  target="_blank"
                                  rel="noreferrer"
                                  style={{ color: "var(--ink)" }}
                                >
                                  Open
                                  <IconExternal size={12} />
                                </a>
                              </>
                            )}
                          </div>
                        </div>
                      );
                    })}

                    <div style={{ borderTop: "1px solid var(--hair)", padding: "10px 16px 10px 30px" }}>
                      {addingDbTo === server.id ? (
                        <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
                          <input
                            className="field"
                            autoFocus
                            style={{ maxWidth: 280, padding: "7px 10px", fontSize: 12.5 }}
                            value={newDbName}
                            placeholder="database_name"
                            onChange={(e) => setNewDbName(e.target.value)}
                            onKeyDown={(e) => {
                              if (e.key === "Enter") void addDatabase(server);
                              if (e.key === "Escape") {
                                setAddingDbTo(null);
                                setNewDbName("");
                              }
                            }}
                          />
                          <button className="btn btn-s btn-p" onClick={() => void addDatabase(server)}>
                            Create
                          </button>
                          <button
                            className="btn btn-s btn-q"
                            onClick={() => {
                              setAddingDbTo(null);
                              setNewDbName("");
                            }}
                          >
                            Cancel
                          </button>
                        </div>
                      ) : (
                        <button
                          className="btn btn-q btn-s"
                          onClick={() => {
                            setAddingDbTo(server.id);
                            setNewDbName("");
                          }}
                        >
                          <IconPlus size={12} strokeWidth={2.2} />
                          Add a database here
                        </button>
                      )}
                    </div>
                  </>
                ) : null}
              </div>
            );
          })
        )}
      </div>

      {wizard && project ? (
        <NewOdooWizard
          api={api}
          projectId={project.id}
          projectName={project.name}
          onCancel={() => setWizard(false)}
          onDone={(result) => {
            setWizard(false);
            setExpanded((prev) => new Set(prev).add(result.server.id));
            void reload();
          }}
        />
      ) : null}

      {confirm ? <ConfirmDialog spec={confirm} onClose={() => setConfirm(null)} /> : null}
    </div>
  );
}
