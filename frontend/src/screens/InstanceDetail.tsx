import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type {
  AddonsSource,
  Database,
  DatabaseModuleState,
  EventEnvelope,
  Freshness,
  LogLine,
  ModuleScan,
  Backup,
  CachedTemplate,
  DatabaseOrigin,
  DiscoveredRoot,
  NeutralizationState,
  OdooServer,
  ReloadPolicy,
  Snapshot,
  StartupPreview,
  UpgradePlan,
  UpgradeRun,
} from "../lib/api";
import { subscribeLogs } from "../lib/api";
import type { ScopedApi, ScopedSubscribeEvents } from "../lib/scopedApi";
import { ConfirmDialog, type ConfirmSpec } from "../components/Confirm";
import { describeError } from "../components/NewOdooWizard";
import { Status, isBusy } from "../components/Status";
import { IconBranch, IconChevronLeft, IconCopy, IconExternal, IconInfo, IconPlus, IconWarn } from "../components/Icons";
import { Banner } from "./Projects";
import { databaseUrl, formatBytes, plural, proxyPort, timeAgo, uptime } from "../lib/format";
import { describeEvent, type NameMaps } from "../lib/eventDescriptions";

/** One Odoo, everything about it in one place.
 *
 *  Tabs rather than more nesting: once you're inside an Odoo, the questions
 *  are all about the same object — what's in it, what code it's running,
 *  what it's doing right now — so making someone navigate away and back to
 *  answer a second one is the window-switch tax the design doc warns about.
 *
 *  Snapshots live in a rail beside the tabs, not inside one, because
 *  reversibility has to be visible *before* an action, not found after it.
 *  That's the whole product thesis: the fear being solved is breaking a
 *  client's database, and a safety net you have to go looking for doesn't
 *  discharge it. */

const TABS = [
  "Databases",
  "Apps",
  "My code",
  "Community",
  "Config",
  "Logs",
  "Debug",
  "Editor",
  "Browser",
] as const;
type Tab = (typeof TABS)[number];

interface Props {
  api: ScopedApi;
  subscribeEvents: ScopedSubscribeEvents;
  serverId: string;
  onBack: () => void;
}

export function InstanceDetail({ api, subscribeEvents, serverId, onBack }: Props) {
  const [server, setServer] = useState<OdooServer | null>(null);
  const [databases, setDatabases] = useState<Database[]>([]);
  const [snapshots, setSnapshots] = useState<Snapshot[]>([]);
  const [sources, setSources] = useState<AddonsSource[]>([]);
  const [events, setEvents] = useState<EventEnvelope[]>([]);
  const [branch, setBranch] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab>("Databases");
  const [error, setError] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<ConfirmSpec | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  const reload = useCallback(async () => {
    try {
      const [servers, dbs, snaps, srcs, evs] = await Promise.all([
        api.listServers(),
        api.listDatabases(serverId),
        api.listSnapshots(),
        api.listAddonsSources(serverId),
        api.recentEvents(200),
      ]);
      const found = servers.find((s) => s.id === serverId) ?? null;
      setServer(found);
      setDatabases(dbs);
      const dbIds = new Set(dbs.map((d) => d.id));
      setSnapshots(snaps.filter((s) => s.server_id === serverId || dbIds.has(s.database_id)));
      setSources(srcs);
      setEvents(evs);
      setError(null);
    } catch (err) {
      setError(String(err));
    }
    // A null branch is a normal answer (nothing linked, or not a git
    // checkout), not a failure — so it must never surface as an error.
    api.getGitBranch(serverId).then((r) => setBranch(r.branch)).catch(() => setBranch(null));
  }, [api, serverId]);

  useEffect(() => {
    void reload();
  }, [reload]);
  useEffect(() => subscribeEvents(() => void reload()), [subscribeEvents, reload]);

  const protectedDbs = useMemo(() => new Set(snapshots.map((s) => s.database_id)), [snapshots]);
  const myEvents = useMemo(() => {
    const dbIds = new Set(databases.map((d) => d.id));
    return events.filter((e) => {
      const p = e.payload as Record<string, unknown>;
      return p.server_id === serverId || (typeof p.database_id === "string" && dbIds.has(p.database_id));
    });
  }, [events, databases, serverId]);

  async function start() {
    if (!server) return;
    setBusy("starting");
    try {
      // First run of a version downloads it, so this can take a while.
      await api.startServer(server.id);
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(null);
      void reload();
    }
  }

  async function stop() {
    if (!server) return;
    setBusy("stopping");
    try {
      await api.stopServer(server.id);
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(null);
      void reload();
    }
  }

  if (!server) {
    return (
      <div style={{ flex: 1, padding: 26 }}>
        <div className="card" style={{ height: 120, opacity: 0.5 }} />
      </div>
    );
  }

  return (
    <div style={{ flex: 1, display: "flex", flexDirection: "column", minWidth: 0 }}>
      <div
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          padding: "16px 26px",
          flexShrink: 0,
          gap: 12,
        }}
      >
        <div style={{ display: "flex", alignItems: "center", gap: 13, minWidth: 0 }}>
          <button className="btn btn-q btn-icon" aria-label="Back to project" onClick={onBack}>
            <IconChevronLeft />
          </button>
          <span className="m" style={{ fontSize: 17, fontWeight: 500, letterSpacing: "-0.02em" }}>
            {server.name}
          </span>
          {busy ? (
            <span
              style={{
                display: "inline-flex",
                alignItems: "center",
                gap: 7,
                fontFamily: "var(--mono)",
                fontSize: 11.5,
                color: "var(--run)",
                background: "var(--run-soft)",
                borderRadius: 5,
                padding: "4px 9px",
              }}
            >
              <span className="dot dot-busy" />
              {busy}
            </span>
          ) : (
            <span
              style={{
                display: "inline-flex",
                alignItems: "center",
                gap: 7,
                background: server.state.status === "running" ? "var(--ok-soft)" : "var(--s2)",
                borderRadius: 5,
                padding: "4px 9px",
              }}
            >
              <Status state={server.state} />
            </span>
          )}
          <span style={{ fontSize: 12, color: "var(--ink4)" }}>
            Odoo {server.odoo_version} · port {server.port}
            {server.state.status === "running" && server.started_at ? ` · up ${uptime(server.started_at)}` : ""}
          </span>
        </div>
        <div style={{ display: "flex", gap: 8, flexShrink: 0 }}>
          {server.state.status === "running" ? (
            <button className="btn" disabled={!!busy} onClick={() => void stop()}>
              Stop
            </button>
          ) : (
            <button className="btn btn-p" disabled={!!busy || isBusy(server.state)} onClick={() => void start()}>
              Start
            </button>
          )}
        </div>
      </div>

      <div
        className="scroll"
        style={{
          display: "flex",
          gap: 24,
          padding: "4px 26px 0",
          borderBottom: "1px solid var(--hair)",
          flexShrink: 0,
          overflowX: "auto",
        }}
      >
        {TABS.map((t) => (
          <button key={t} className={`tab${tab === t ? " on" : ""}`} onClick={() => setTab(t)}>
            {t}
          </button>
        ))}
      </div>

      {error ? (
        <div style={{ paddingTop: 14 }}>
          <Banner tone="err" onDismiss={() => setError(null)}>
            {error}
          </Banner>
        </div>
      ) : null}

      <div style={{ flex: 1, display: "flex", minHeight: 0 }}>
        <div className="scroll" style={{ flex: 1, padding: "20px 26px 26px", minWidth: 0 }}>
          {tab === "Databases" ? (
            <DatabasesTab
              api={api}
              server={server}
              databases={databases}
              protectedDbs={protectedDbs}
              onError={setError}
              onConfirm={setConfirm}
              onChanged={reload}
            />
          ) : null}
          {tab === "Apps" ? (
            <AppsTab api={api} server={server} databases={databases} onError={setError} onChanged={reload} />
          ) : null}
          {tab === "My code" ? (
            <CodeTab
              api={api}
              server={server}
              sources={sources.filter((s) => s.kind === "private")}
              kind="private"
              branch={branch}
              onError={setError}
              onChanged={reload}
            />
          ) : null}
          {tab === "Community" ? (
            <CodeTab
              api={api}
              server={server}
              sources={sources.filter((s) => s.kind !== "private")}
              kind="oca"
              branch={null}
              onError={setError}
              onChanged={reload}
            />
          ) : null}
          {tab === "Config" ? <ConfigTab api={api} server={server} databases={databases} onError={setError} onChanged={reload} /> : null}
          {tab === "Logs" ? <LogsTab api={api} events={myEvents} server={server} databases={databases} snapshots={snapshots} /> : null}
          {tab === "Debug" ? (
            <DebugTab api={api} server={server} databases={databases} onError={setError} onChanged={reload} />
          ) : null}
          {tab === "Editor" ? <NotWiredYet which="editor" /> : null}
          {tab === "Browser" ? <NotWiredYet which="browser" /> : null}
        </div>

        <SnapshotRail
          api={api}
          server={server}
          databases={databases}
          snapshots={snapshots}
          onError={setError}
          onConfirm={setConfirm}
          onChanged={reload}
        />
      </div>

      {confirm ? <ConfirmDialog spec={confirm} onClose={() => setConfirm(null)} /> : null}
    </div>
  );
}

// --- Databases -------------------------------------------------------------

function DatabasesTab({
  api,
  server,
  databases,
  protectedDbs,
  onError,
  onConfirm,
  onChanged,
}: {
  api: ScopedApi;
  server: OdooServer;
  databases: Database[];
  protectedDbs: Set<string>;
  onError: (m: string) => void;
  onConfirm: (c: ConfirmSpec) => void;
  onChanged: () => void;
}) {
  const [adding, setAdding] = useState(false);
  const [name, setName] = useState("");
  // Whether the new database should arrive as a working Odoo database or
  // as a bare Postgres one. Default: working — a "new database" that no
  // Odoo has touched isn't a database you can log into, and that surprised
  // everyone who tried it.
  const [readyToUse, setReadyToUse] = useState(true);
  const [templates, setTemplates] = useState<CachedTemplate[]>([]);

  const reloadTemplates = useCallback(() => {
    void api
      .listTemplates()
      .then(setTemplates)
      .catch(() => setTemplates([]));
  }, [api]);

  useEffect(reloadTemplates, [reloadTemplates, databases.length]);
  const [pending, setPending] = useState<Record<string, string>>({});
  const [backups, setBackups] = useState<Backup[]>([]);
  // Which backup is being restored, and under what name. Restoring never
  // overwrites: it always makes a new database, so there is always a name
  // to choose and nothing to lose by getting it wrong.
  const [restoring, setRestoring] = useState<{ backup: Backup; name: string; neutralize: boolean } | null>(null);
  const [neutralization, setNeutralization] = useState<Record<string, NeutralizationState>>({});

  const reloadNeutralization = useCallback(() => {
    void api
      .neutralization()
      .then(setNeutralization)
      .catch(() => setNeutralization({}));
  }, [api]);

  useEffect(reloadNeutralization, [reloadNeutralization, databases.length]);

  const reloadBackups = useCallback(() => {
    void api
      .listBackups()
      .then(setBackups)
      .catch(() => setBackups([]));
  }, [api]);

  useEffect(reloadBackups, [reloadBackups, databases.length]);

  function mark(id: string, label: string | null) {
    setPending((p) => {
      const next = { ...p };
      if (label === null) delete next[id];
      else next[id] = label;
      return next;
    });
  }

  async function run(id: string, label: string, fn: () => Promise<unknown>) {
    mark(id, label);
    try {
      await fn();
    } catch (err) {
      onError(describeError(err));
    } finally {
      mark(id, null);
      // Backing up doesn't change the database list, so refreshing on
      // `onChanged` alone would leave a just-taken backup invisible until
      // something else happened.
      reloadBackups();
      reloadNeutralization();
      onChanged();
    }
  }

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
      {databases.length > 0 ? (
        <div className="card" style={{ padding: "20px 22px", display: "flex", alignItems: "center", gap: 22 }}>
          <div style={{ flex: 1, minWidth: 0 }}>
            <div className="cap" style={{ marginBottom: 8 }}>
              Your Odoo is at
            </div>
            <div className="m" style={{ fontSize: 21, letterSpacing: "-0.02em", color: "var(--ink)" }}>
              {databaseUrl(databases[0], proxyPort())}
            </div>
          </div>
          <button
            className="btn btn-q btn-s"
            onClick={() => void navigator.clipboard?.writeText(`http://${databaseUrl(databases[0], proxyPort())}`)}
          >
            <IconCopy />
            Copy link
          </button>
          <a
            className="btn btn-p"
            style={{ padding: "11px 20px", fontSize: 13.5 }}
            href={`http://${databaseUrl(databases[0], proxyPort())}`}
            target="_blank"
            rel="noreferrer"
          >
            Open in browser
            <IconExternal />
          </a>
        </div>
      ) : null}

      <div className="card" style={{ overflow: "hidden" }}>
        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            padding: "14px 18px",
          }}
        >
          <span className="cap">Databases on this Odoo</span>
          <button className="btn btn-q btn-s" onClick={() => setAdding(true)}>
            <IconPlus size={12} strokeWidth={2.2} />
            New database
          </button>
        </div>

        {adding ? (
          <div style={{ padding: "0 18px 14px" }}>
          <div style={{ display: "flex", gap: 8 }}>
            <input
              className="field"
              autoFocus
              style={{ maxWidth: 280, padding: "7px 10px", fontSize: 12.5 }}
              value={name}
              placeholder="database_name"
              onChange={(e) => setName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Escape") {
                  setAdding(false);
                  setName("");
                }
              }}
            />
            <button
              className="btn btn-s btn-p"
              disabled={!name.trim()}
              onClick={() =>
                void run("new", readyToUse && templates.length === 0 ? "installing Odoo — first one only" : "creating", async () => {
                  await api.createDatabase(server.id, name.trim(), readyToUse ? ["base"] : []);
                  setName("");
                  setAdding(false);
                  reloadTemplates();
                })
              }
            >
              Create
            </button>
            <button
              className="btn btn-s btn-q"
              onClick={() => {
                setAdding(false);
                setName("");
              }}
            >
              Cancel
            </button>
          </div>

          {/* The honest version of "instant". The first database of a
              shape really does boot Odoo, and saying so beforehand is
              better than a spinner that surprises somebody. */}
          <label style={{ display: "flex", alignItems: "flex-start", gap: 9, marginTop: 11, cursor: "pointer" }}>
            <input
              type="checkbox"
              checked={readyToUse}
              onChange={(e) => setReadyToUse(e.target.checked)}
              style={{ marginTop: 1 }}
            />
            <span style={{ flex: 1 }}>
              <span style={{ fontSize: 12.5, color: "var(--ink)" }}>Ready to log into</span>
              <span style={{ fontSize: 11.5, color: "var(--ink4)", display: "block", marginTop: 2, lineHeight: 1.55 }}>
                {readyToUse ? (
                  templates.length > 0 ? (
                    <>
                      Copied from the one already built — about a second. Without this you get a bare Postgres
                      database that Odoo hasn&rsquo;t initialized yet.
                    </>
                  ) : (
                    <>
                      The first one runs a real Odoo install, so give it a minute. Every database after it is copied
                      from that and takes about a second.
                    </>
                  )
                ) : (
                  <>An empty Postgres database. Odoo hasn&rsquo;t touched it, so there&rsquo;s nothing to log into yet.</>
                )}
              </span>
            </span>
          </label>
          </div>
        ) : null}

        {databases.length === 0 ? (
          <div style={{ padding: "18px", borderTop: "1px solid var(--hair)", fontSize: 12.5, color: "var(--ink3)" }}>
            No databases yet. Create one and it'll be reachable the moment this Odoo is running.
          </div>
        ) : (
          databases.map((db) => {
            const p = pending[db.id];
            return (
              <div className="db db-wide" key={db.id}>
                <div style={{ display: "flex", alignItems: "center", gap: 11, minWidth: 0 }}>
                  <span className={`dot ${server.state.status === "running" ? "dot-run" : "dot-stop"}`} />
                  <span className="m" style={{ fontSize: 12.5, color: "var(--ink)" }}>
                    {db.name}
                  </span>
                  {!protectedDbs.has(db.id) ? (
                    <span style={{ fontSize: 11, color: "var(--warn)" }}>no snapshot</span>
                  ) : null}
                  <LiveDataBadge
                    origin={db.origin}
                    state={neutralization[db.id]}
                    onNeutralize={() => void run(db.id, "neutralizing", () => api.neutralizeDatabase(db.id))}
                  />
                </div>
                <span className="m" style={{ fontSize: 11.5, color: "var(--ink3)" }}>
                  {databaseUrl(db, proxyPort())}
                </span>
                <span className="m" style={{ fontSize: 11.5, color: "var(--ink4)" }}>
                  {formatBytes(db.size_bytes)}
                </span>
                <div style={{ display: "flex", gap: 6, justifyContent: "flex-end" }}>
                  {p ? (
                    <span style={{ display: "inline-flex", alignItems: "center", gap: 6, fontSize: 11, color: "var(--ink3)" }}>
                      <span className="dot dot-busy" />
                      <span className="m">{p}</span>
                    </span>
                  ) : (
                    <>
                      <button
                        className="btn btn-s btn-q"
                        onClick={() =>
                          void run(db.id, "copying", () => api.duplicateDatabase(db.id, `${db.name}_copy`))
                        }
                      >
                        Duplicate
                      </button>
                      <button className="btn btn-s btn-q" onClick={() => void run(db.id, "backing up", () => api.backupDatabase(db.id))}>
                        Back up
                      </button>
                      <button
                        className="btn btn-s btn-q"
                        style={{ color: "var(--ink4)" }}
                        onClick={() =>
                          onConfirm({
                            rung: 4,
                            title: "Drop this database?",
                            object: db.name,
                            consequence: `${formatBytes(db.size_bytes)}, dropped from Postgres for good`,
                            reversibility: protectedDbs.has(db.id)
                              ? "A snapshot of this database exists, so the data can be brought back — but this database and its address go now."
                              : "There is no snapshot of this database. Once it's dropped, its data is gone.",
                            ripple: "Anything pointed at its address stops working immediately.",
                            actionLabel: `Drop ${db.name}`,
                            onConfirm: () => {
                              onConfirm satisfies unknown;
                              void run(db.id, "dropping", () => api.dropDatabase(db.id));
                            },
                          })
                        }
                      >
                        Drop
                      </button>
                    </>
                  )}
                </div>
              </div>
            );
          })
        )}
      </div>

      {/* --- backups ----------------------------------------------------------
          "Back up" wrote dumps that nothing in this app could ever find
          again — the restore call existed but only somebody who knew the
          file path could reach it, which is nobody. A backup you can't
          restore from inside the tool that took it is a button, not a
          safety net. */}
      {backups.length > 0 ? (
        <div className="card" style={{ overflow: "hidden" }}>
          <div style={{ padding: "14px 18px", borderBottom: "1px solid var(--hair)" }}>
            <span className="cap">Backups on this machine</span>
            <div style={{ fontSize: 11.5, color: "var(--ink4)", marginTop: 4, lineHeight: 1.6 }}>
              Restoring makes a <em>new</em> database — nothing here overwrites what you have.
            </div>
          </div>
          {backups.map((backup) => (
            <div key={backup.path} className="db db-wide">
              <div style={{ display: "flex", alignItems: "center", gap: 10, minWidth: 0 }}>
                <span className="m" style={{ fontSize: 12.5, color: "var(--ink)" }}>
                  {backup.database_name}
                </span>
                {!backup.source_still_exists ? (
                  <span style={{ fontSize: 11, color: "var(--warn)" }}>original is gone</span>
                ) : null}
              </div>
              <span style={{ fontSize: 11.5, color: "var(--ink3)" }}>{timeAgo(backup.taken_at)}</span>
              <span className="m" style={{ fontSize: 11.5, color: "var(--ink4)" }}>
                {formatBytes(backup.size_bytes)}
                {backup.has_filestore ? " · with attachments" : " · rows only"}
              </span>
              <div style={{ display: "flex", gap: 6, justifyContent: "flex-end" }}>
                {pending[backup.path] ? (
                  <span style={{ display: "inline-flex", alignItems: "center", gap: 6, fontSize: 11, color: "var(--ink3)" }}>
                    <span className="dot dot-busy" />
                    <span className="m">{pending[backup.path]}</span>
                  </span>
                ) : (
                  <button
                    className="btn btn-s btn-q"
                    onClick={() => setRestoring({ backup, name: `${backup.database_name}_restored`, neutralize: true })}
                  >
                    Restore as…
                  </button>
                )}
              </div>
            </div>
          ))}

          {restoring ? (
            <div style={{ padding: "14px 18px", borderTop: "1px solid var(--hair)", background: "var(--s2)" }}>
              <div style={{ fontSize: 12, color: "var(--ink2)", marginBottom: 9 }}>
                Restore <span className="m">{restoring.backup.database_name}</span> into a new database called:
              </div>
              <div style={{ display: "flex", gap: 8 }}>
                <input
                  className="field m"
                  autoFocus
                  spellCheck={false}
                  value={restoring.name}
                  onChange={(e) => setRestoring({ ...restoring, name: e.target.value })}
                />
                <button
                  className="btn btn-s"
                  disabled={
                    restoring.name.trim() === "" || databases.some((d) => d.name === restoring.name.trim())
                  }
                  onClick={() => {
                    const { backup, name, neutralize } = restoring;
                    setRestoring(null);
                    void run(backup.path, neutralize ? "restoring + neutralizing" : "restoring", () =>
                      api.restoreDatabase(server.id, name.trim(), backup.path, neutralize),
                    );
                  }}
                >
                  Restore
                </button>
                <button className="btn btn-s btn-q" onClick={() => setRestoring(null)}>
                  Cancel
                </button>
              </div>
              {databases.some((d) => d.name === restoring.name.trim()) ? (
                <div style={{ fontSize: 11.5, color: "var(--warn)", marginTop: 8 }}>
                  There's already a database called that here. Pick another name — this won't replace it.
                </div>
              ) : null}

              {/* On by default, and unchecking it is the deliberate act.
                  A dump from a client carries their outgoing mail servers
                  *with working passwords*, every scheduled job, and live
                  payment credentials — so a copy left running can email
                  their customers and charge real cards from your laptop. */}
              <label
                style={{
                  display: "flex",
                  alignItems: "flex-start",
                  gap: 10,
                  marginTop: 12,
                  padding: "11px 13px",
                  borderRadius: 9,
                  cursor: "pointer",
                  background: restoring.neutralize ? "var(--ok-soft)" : "var(--warn-soft, var(--s2))",
                  border: `1px solid ${restoring.neutralize ? "oklch(0.72 0.15 155 / 0.28)" : "var(--warn)"}`,
                }}
              >
                <input
                  type="checkbox"
                  checked={restoring.neutralize}
                  onChange={(e) => setRestoring({ ...restoring, neutralize: e.target.checked })}
                  style={{ marginTop: 1, accentColor: "var(--ok)" }}
                />
                <span style={{ flex: 1 }}>
                  <span style={{ fontSize: 12.5, color: "var(--ink)", display: "block", marginBottom: 3 }}>
                    Neutralize it first
                  </span>
                  <span style={{ fontSize: 11.5, color: "var(--ink3)", lineHeight: 1.55, display: "block" }}>
                    {restoring.neutralize ? (
                      <>
                        Runs Odoo&rsquo;s own neutralization: outgoing mail servers switched off and their passwords
                        wiped, scheduled jobs stopped, payment credentials cleared.
                      </>
                    ) : (
                      <>
                        Leaving this off means the copy keeps the client&rsquo;s working mail server passwords, live
                        scheduled jobs and real payment credentials. It can email their customers from this machine.
                      </>
                    )}
                  </span>
                </span>
              </label>
            </div>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

/** Says whether a database can still reach the outside world — but only
 *  where that's news.
 *
 *  A database you made here is empty and harmless, so badging every one of
 *  them "not neutralized" would be noise that teaches people to ignore the
 *  word. The warning is for copies whose contents came from somewhere else:
 *  a restored dump or an adopted database. That's what `origin` is for.
 *
 *  "Couldn't check" is never drawn as either answer. */
function LiveDataBadge({
  origin,
  state,
  onNeutralize,
}: {
  origin: DatabaseOrigin;
  state: NeutralizationState | undefined;
  onNeutralize: () => void;
}) {
  if (state === "neutralized") {
    return (
      <span style={{ fontSize: 11, color: "var(--ok)" }} title="Odoo's neutralization has been run: no mail, no crons, no live payment credentials.">
        neutralized
      </span>
    );
  }
  // Nothing to say about a database no Odoo has initialized, one still
  // being checked, or one whose contents this machine made itself.
  if (state !== "live" || !origin || !["restored", "adopted"].includes(origin)) return null;
  return (
    <span style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
      <span
        style={{ fontSize: 11, color: "var(--warn)" }}
        title="This data came from somewhere else and can still send mail, run scheduled jobs and use real payment credentials."
      >
        live data
      </span>
      <button className="btn btn-q btn-s" style={{ fontSize: 10.5, padding: "1px 7px" }} onClick={onNeutralize}>
        Neutralize
      </button>
    </span>
  );
}

// --- Apps ------------------------------------------------------------------

function AppsTab({
  api,
  server,
  databases,
  onError,
  onChanged,
}: {
  api: ScopedApi;
  server: OdooServer;
  databases: Database[];
  onError: (m: string) => void;
  onChanged: () => void;
}) {
  const [scan, setScan] = useState<ModuleScan | null>(null);
  const [scanning, setScanning] = useState(false);
  const [target, setTarget] = useState<string>(databases[0]?.id ?? "");
  /** The database's own `ir_module_module` rows, keyed by module name —
   *  the whole row, so the installed version is available and not just
   *  the state. */
  const [states, setStates] = useState<Record<string, DatabaseModuleState>>({});
  const [freshness, setFreshness] = useState<Record<string, Freshness>>({});
  const [showEverything, setShowEverything] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [query, setQuery] = useState("");

  useEffect(() => {
    if (!target && databases[0]) setTarget(databases[0].id);
  }, [databases, target]);

  useEffect(() => {
    let alive = true;
    setScanning(true);
    api
      .scanModules(server.id)
      .then((s) => alive && setScan(s))
      .catch((err) => alive && onError(describeError(err)))
      .finally(() => alive && setScanning(false));
    return () => {
      alive = false;
    };
  }, [api, server.id, onError]);

  useEffect(() => {
    if (!target) return;
    let alive = true;
    api
      .getDatabaseModuleStates(target)
      .then((rows) => {
        if (!alive) return;
        setStates(Object.fromEntries(rows.map((r) => [r.technical_name, r])));
      })
      // An empty/failed read here is normal for a database that has never
      // had a module installed, or one whose Odoo isn't running — the tab
      // still lists what's available, it just can't say what's installed.
      .catch(() => alive && setStates({}));
    return () => {
      alive = false;
    };
  }, [api, target]);

  const reloadFreshness = useCallback(() => {
    if (!target) {
      setFreshness({});
      return;
    }
    void api
      .moduleFreshness(target)
      .then((rows) => setFreshness(Object.fromEntries(rows.map((r) => [r.technical_name, r.freshness]))))
      // Same posture as the module states above: a database whose Odoo has
      // never run can't answer this, and the tab still works without it.
      .catch(() => setFreshness({}));
  }, [api, target]);

  useEffect(reloadFreshness, [reloadFreshness]);

  const apps = useMemo(() => {
    const all = scan?.modules ?? [];
    // `application: true` is Odoo's flag for the big top-level apps (Sales,
    // Inventory). Almost nobody's own module sets it — so filtering on it
    // alone made a developer's own code invisible on the one screen meant
    // for installing it. Your own folders are shown by default; Odoo's
    // thousand core modules stay behind the toggle.
    const visible = all.filter((m) => {
      if (m.shadowed) return false;
      if (showEverything) return true;
      return m.application || m.source_kind !== "core";
    });
    const q = query.trim().toLowerCase();
    return q ? visible.filter((m) => m.name.toLowerCase().includes(q) || m.technical_name.includes(q)) : visible;
  }, [scan, query, showEverything]);

  /// What the on-disk manifest version and the installed version say when
  /// compared. Deliberately narrow: this compares two *declared version
  /// numbers*, which is a real signal and not the whole answer — editing
  /// Python without bumping the manifest changes neither. The UI says so
  /// rather than claiming to know whether your code is live.
  // Content hashes, not manifest versions. Comparing the declared version
  // only notices a change when somebody remembered to bump it, and nobody
  // bumps it while iterating — which is exactly when you most want to know
  // whether the code you just wrote is the code that's running.
  const changed = apps.filter((m) => freshness[m.technical_name] === "changed");
  // Installed before this app started recording, or by something else. Not
  // "fine" — unknown, and drawn as its own thing rather than folded into
  // either answer.
  const unrecorded = apps.filter((m) => freshness[m.technical_name] === "unrecorded");

  /** Updates every module whose declared version differs from what is
   *  installed, one at a time so a failure names the module that failed
   *  rather than the whole batch. */
  /** Updates every changed module in **one** Odoo boot.
   *
   *  This used to run one `-u` per module. Odoo loads its whole registry on
   *  each of those, and that load is nearly all of the time — so ten
   *  changed modules cost ten boots to do what one can. The backend falls
   *  back to one at a time if the batch fails, so a failure still names the
   *  module that caused it. */
  async function updateAllStale() {
    if (!target || changed.length === 0) return;
    setBusy(changed.length === 1 ? changed[0].technical_name : `${changed.length} modules`);
    try {
      await api.upgradeModulesBatched(
        target,
        changed.map((m) => m.technical_name),
      );
    } catch (err) {
      onError(describeError(err));
    }
    try {
      const rows = await api.getDatabaseModuleStates(target);
      setStates(Object.fromEntries(rows.map((r) => [r.technical_name, r])));
      reloadFreshness();
    } catch {
      // The states stay as they were; any failure above already said so.
    }
    setBusy(null);
    onChanged();
  }

  async function act(kind: "install" | "upgrade" | "uninstall", technicalName: string) {
    if (!target) return;
    setBusy(technicalName);
    try {
      if (kind === "install") await api.installModules(target, [technicalName]);
      else if (kind === "upgrade") await api.upgradeModules(target, [technicalName]);
      else await api.uninstallModules(target, [technicalName]);
      const rows = await api.getDatabaseModuleStates(target);
      setStates(Object.fromEntries(rows.map((r) => [r.technical_name, r])));
    } catch (err) {
      onError(describeError(err));
    } finally {
      setBusy(null);
      onChanged();
    }
  }

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
      <div style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap" }}>
        <input
          className="field"
          style={{ maxWidth: 260, padding: "7px 11px", fontSize: 12.5, fontFamily: "var(--ui)" }}
          value={query}
          placeholder="Search apps"
          onChange={(e) => setQuery(e.target.value)}
        />
        {databases.length > 0 ? (
          <label style={{ display: "flex", alignItems: "center", gap: 8, fontSize: 12, color: "var(--ink3)" }}>
            on
            <select
              value={target}
              onChange={(e) => setTarget(e.target.value)}
              style={{
                background: "var(--s2)",
                color: "var(--ink2)",
                border: "1px solid var(--hair2)",
                borderRadius: 6,
                padding: "6px 8px",
                fontFamily: "var(--mono)",
                fontSize: 12,
              }}
            >
              {databases.map((d) => (
                <option key={d.id} value={d.id}>
                  {d.name}
                </option>
              ))}
            </select>
          </label>
        ) : null}
        <label style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 12, color: "var(--ink3)" }}>
          <input type="checkbox" checked={showEverything} onChange={(e) => setShowEverything(e.target.checked)} />
          Include Odoo's own modules
        </label>
        <span style={{ fontSize: 11.5, color: "var(--ink4)" }}>
          {scanning ? "scanning the module folders…" : `${plural(apps.length, "module")} available`}
        </span>
      </div>

      {unrecorded.length > 0 && changed.length === 0 && (
        <div className="card" style={{ padding: "11px 15px", display: "flex", gap: 11, alignItems: "flex-start" }}>
          <span style={{ color: "var(--ink4)", display: "flex", marginTop: 1 }}>
            <IconInfo />
          </span>
          <div style={{ fontSize: 11.5, color: "var(--ink3)", lineHeight: 1.55 }}>
            {plural(unrecorded.length, "module was", "modules were")} installed before this app started tracking them,
            so it can&rsquo;t tell whether the code on disk is what&rsquo;s running. Updating one settles it.
          </div>
        </div>
      )}

      {changed.length > 0 && (
        <div
          className="card"
          style={{
            padding: "12px 15px",
            display: "flex",
            alignItems: "center",
            gap: 12,
            borderColor: "var(--warn)",
            background: "var(--warn-soft)",
          }}
        >
          <span style={{ color: "var(--warn)", display: "flex" }}>
            <IconWarn size={14} />
          </span>
          <div style={{ flex: 1, fontSize: 12.5, color: "var(--ink2)", lineHeight: 1.5 }}>
            <b style={{ fontWeight: 560 }}>
              {changed.length === 1
                ? "One module's code has changed since it was installed here"
                : `${changed.length} modules' code has changed since they were installed here`}
            </b>
            <div className="m" style={{ fontSize: 11.5, color: "var(--ink3)", marginTop: 3 }}>
              {changed.map((m) => m.technical_name).join(", ")}
            </div>
            <div style={{ fontSize: 11, color: "var(--ink4)", marginTop: 4 }}>
              Compared by file contents, so this catches edits that didn&rsquo;t bump a version.
            </div>
          </div>
          <button className="btn btn-s btn-p" disabled={!target || busy !== null} onClick={() => void updateAllStale()}>
            Update {changed.length === 1 ? "it" : `all ${changed.length}`}
          </button>
        </div>
      )}

      {scan && scan.source_errors.length > 0 ? (
        <div
          style={{
            background: "var(--warn-soft)",
            border: "1px solid oklch(0.79 0.13 78 / 0.3)",
            borderRadius: 9,
            padding: "12px 15px",
            display: "flex",
            gap: 11,
          }}
        >
          <span style={{ color: "var(--warn)", display: "flex", marginTop: 1 }}>
            <IconWarn size={14} />
          </span>
          <div style={{ fontSize: 12, color: "var(--ink2)", lineHeight: 1.6 }}>
            {scan.source_errors.map((e) => (
              <div key={e.source_id}>
                <span className="m">{e.path_or_url}</span> couldn't be read — {e.message}
              </div>
            ))}
          </div>
        </div>
      ) : null}

      <div className="card" style={{ overflow: "hidden" }}>
        {apps.length === 0 && !scanning ? (
          <div style={{ padding: 18, fontSize: 12.5, color: "var(--ink3)" }}>
            Nothing to install yet. Point this Odoo at a folder of modules under <b>My code</b> or <b>Community</b>,
            and they'll show up here.
          </div>
        ) : (
          apps.map((m) => {
            const row = states[m.technical_name];
            const state = row?.state;
            const installed = state === "installed";
            const isStale =
              installed && !!row?.installed_version && !!m.version && row.installed_version !== m.version;
            return (
              <div
                key={m.technical_name}
                className="comp"
                style={{ gridTemplateColumns: "1fr 130px 96px 168px" }}
              >
                <div style={{ minWidth: 0 }}>
                  <div style={{ fontSize: 12.5, color: "var(--ink)" }}>{m.name}</div>
                  <div className="m" style={{ fontSize: 11, color: "var(--ink4)", marginTop: 2 }}>
                    {m.technical_name}
                    {m.version ? ` · ${m.version}` : ""}
                  </div>
                </div>
                <span className="badge badge-quiet" style={{ justifySelf: "start" }}>
                  {m.source_label}
                </span>
                <span
                  className="m"
                  style={{ fontSize: 11.5, color: isStale ? "var(--warn)" : installed ? "var(--ok)" : "var(--ink4)" }}
                  title={
                    isStale
                      ? `Installed ${row?.installed_version}, on disk ${m.version}`
                      : installed && row?.installed_version
                        ? `Installed ${row.installed_version}`
                        : undefined
                  }
                >
                  {isStale ? `${row?.installed_version} → ${m.version}` : (state ?? "not installed")}
                </span>
                <div style={{ display: "flex", gap: 6, justifyContent: "flex-end" }}>
                  {busy === m.technical_name ? (
                    <span style={{ display: "inline-flex", alignItems: "center", gap: 6, fontSize: 11, color: "var(--ink3)" }}>
                      <span className="dot dot-busy" />
                      <span className="m">working</span>
                    </span>
                  ) : installed ? (
                    <>
                      <button
                        className={`btn btn-s${isStale ? " btn-p" : " btn-q"}`}
                        onClick={() => void act("upgrade", m.technical_name)}
                        title={isStale ? "The version on disk isn't the version installed" : "Reload this module from disk"}
                      >
                        Update
                      </button>
                      <button className="btn btn-s btn-q" style={{ color: "var(--ink4)" }} onClick={() => void act("uninstall", m.technical_name)}>
                        Remove
                      </button>
                    </>
                  ) : (
                    <button className="btn btn-s" disabled={!target} onClick={() => void act("install", m.technical_name)}>
                      Install
                    </button>
                  )}
                </div>
              </div>
            );
          })
        )}
      </div>

      <div style={{ fontSize: 11.5, color: "var(--ink4)", lineHeight: 1.6, maxWidth: 720 }}>
        The version shown is the one declared in each module's manifest, compared against the version recorded in this
        database. A difference definitely means an update is due. The reverse isn't a guarantee: editing Python or XML
        without bumping the manifest version changes neither number, and Python changes need this Odoo restarted
        rather than the module updated. This says what it can actually check.
      </div>
    </div>
  );
}

// --- Code (mine / community) ----------------------------------------------

function CodeTab({
  api,
  server,
  sources,
  kind,
  branch,
  onError,
  onChanged,
}: {
  api: ScopedApi;
  server: OdooServer;
  sources: AddonsSource[];
  kind: "private" | "oca";
  branch: string | null;
  onError: (m: string) => void;
  onChanged: () => void;
}) {
  const [adding, setAdding] = useState(false);
  const [label, setLabel] = useState("");
  const [path, setPath] = useState("");
  const [scan, setScan] = useState<ModuleScan | null>(null);
  // Discovery: point at a folder, see what's actually in it. Registering
  // module folders one at a time with a hand-typed priority number is the
  // part of setup people got wrong, and a wrong order silently changes
  // which copy of a module Odoo loads.
  const [scanned, setScanned] = useState<string | null>(null);
  const [discovered, setDiscovered] = useState<DiscoveredRoot[] | null>(null);
  const [chosen, setChosen] = useState<Set<string>>(new Set());
  const [looking, setLooking] = useState(false);

  const alreadyRegistered = useMemo(() => new Set(sources.map((s) => s.path_or_url)), [sources]);

  async function look() {
    if (!path.trim()) return;
    setLooking(true);
    setDiscovered(null);
    try {
      const found = await api.discoverAddons(path.trim());
      setScanned(path.trim());
      setDiscovered(found);
      // Everything not already registered starts ticked: the proposal is
      // the useful default, and unticking is easier than picking.
      setChosen(new Set(found.map((r) => r.path).filter((p) => !alreadyRegistered.has(p))));
    } catch (err) {
      onError(describeError(err));
    } finally {
      setLooking(false);
    }
  }

  async function adopt() {
    if (!scanned || chosen.size === 0) return;
    try {
      await api.adoptAddonsRoots(server.id, scanned, [...chosen]);
      setDiscovered(null);
      setScanned(null);
      setPath("");
      setAdding(false);
      onChanged();
    } catch (err) {
      onError(describeError(err));
    }
  }

  useEffect(() => {
    let alive = true;
    api
      .scanModules(server.id)
      .then((s) => alive && setScan(s))
      .catch(() => undefined);
    return () => {
      alive = false;
    };
  }, [api, server.id]);

  async function add() {
    if (!label.trim() || !path.trim()) return;
    try {
      // Rank orders the addons path: lower wins a name collision. New
      // private code goes first, community after whatever already exists.
      const rank = kind === "private" ? 0 : sources.length + 1;
      await api.createAddonsSource(server.id, label.trim(), path.trim(), kind, rank);
      setLabel("");
      setPath("");
      setAdding(false);
      onChanged();
    } catch (err) {
      onError(describeError(err));
    }
  }

  const collisions = scan?.collisions ?? [];

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
      {kind === "private" && branch ? (
        <div className="card" style={{ padding: "14px 18px", display: "flex", alignItems: "center", gap: 12 }}>
          <span className="cap">On branch</span>
          <span className="m" style={{ fontSize: 13, color: "var(--acc-ink)" }}>
            {branch}
          </span>
          <span style={{ fontSize: 11.5, color: "var(--ink4)" }}>
            snapshots you take are named after this by default
          </span>
        </div>
      ) : null}

      <div className="card" style={{ overflow: "hidden" }}>
        <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", padding: "14px 18px" }}>
          <span className="cap">{kind === "private" ? "Your module folders" : "Community & third-party"}</span>
          <button className="btn btn-q btn-s" onClick={() => setAdding(true)}>
            <IconPlus size={12} strokeWidth={2.2} />
            Add a folder
          </button>
        </div>

        {adding ? (
          <div style={{ display: "flex", gap: 8, padding: "0 18px 14px", flexWrap: "wrap" }}>
            <input
              className="field"
              autoFocus
              style={{ maxWidth: 160, padding: "7px 10px", fontSize: 12.5, fontFamily: "var(--ui)" }}
              value={label}
              placeholder="Label"
              onChange={(e) => setLabel(e.target.value)}
            />
            <input
              className="field"
              style={{ flex: 1, minWidth: 220, padding: "7px 10px", fontSize: 12.5 }}
              value={path}
              placeholder="/Users/you/work/acme/addons"
              onChange={(e) => setPath(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && void add()}
            />
            <button className="btn btn-s" disabled={!path.trim() || looking} onClick={() => void look()}>
              {looking ? "Looking…" : "Look inside"}
            </button>
            <button className="btn btn-s btn-p" disabled={!label.trim() || !path.trim()} onClick={() => void add()}>
              Add
            </button>
            <button
              className="btn btn-s btn-q"
              onClick={() => {
                setAdding(false);
                setDiscovered(null);
              }}
            >
              Cancel
            </button>
          </div>
        ) : null}

        {discovered ? (
          <div style={{ padding: "0 18px 16px" }}>
            {discovered.length === 0 ? (
              <div style={{ fontSize: 12.5, color: "var(--ink3)", lineHeight: 1.6 }}>
                Nothing in there holds modules. An addons folder is one with module directories directly inside it —
                that&rsquo;s Odoo&rsquo;s own rule, not this app&rsquo;s.
              </div>
            ) : (
              <>
                <div style={{ fontSize: 11.5, color: "var(--ink4)", lineHeight: 1.6, marginBottom: 10 }}>
                  {plural(discovered.length, "addons folder")} found, in the order they&rsquo;d load — your own code
                  first, then community, then Odoo&rsquo;s own. Earlier folders win a name collision, which is the
                  whole reason the order matters.
                </div>
                {discovered.map((root, index) => {
                  const already = alreadyRegistered.has(root.path);
                  return (
                    <label
                      key={root.path}
                      style={{
                        display: "flex",
                        alignItems: "flex-start",
                        gap: 10,
                        padding: "9px 0",
                        borderTop: index === 0 ? "none" : "1px solid var(--hair)",
                        cursor: already ? "default" : "pointer",
                        opacity: already ? 0.55 : 1,
                      }}
                    >
                      <input
                        type="checkbox"
                        disabled={already}
                        checked={already || chosen.has(root.path)}
                        onChange={(e) =>
                          setChosen((prev) => {
                            const next = new Set(prev);
                            if (e.target.checked) next.add(root.path);
                            else next.delete(root.path);
                            return next;
                          })
                        }
                        style={{ marginTop: 2 }}
                      />
                      <span style={{ flex: 1, minWidth: 0 }}>
                        <span style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
                          <span style={{ fontSize: 12.5, color: "var(--ink)" }}>{root.label}</span>
                          <span className="badge badge-quiet">{root.kind}</span>
                          <span style={{ fontSize: 11, color: "var(--ink4)" }}>
                            {plural(root.module_count, "module")}
                          </span>
                          {already ? <span style={{ fontSize: 11, color: "var(--ink4)" }}>already added</span> : null}
                        </span>
                        <span
                          className="m"
                          style={{ fontSize: 11, color: "var(--ink4)", display: "block", marginTop: 2, overflowWrap: "anywhere" }}
                        >
                          {root.path}
                        </span>
                        <span style={{ fontSize: 11, color: "var(--ink4)", display: "block", marginTop: 3 }}>
                          {/* The reason, always. A guess nobody can check is
                              a guess they have to trust. */}
                          {root.kind} — {root.why}. {root.sample.join(", ")}
                          {root.module_count > root.sample.length ? "…" : ""}
                        </span>
                      </span>
                    </label>
                  );
                })}
                <button
                  className="btn btn-s btn-p"
                  style={{ marginTop: 12 }}
                  disabled={chosen.size === 0}
                  onClick={() => void adopt()}
                >
                  Add {plural(chosen.size, "folder")}
                </button>
              </>
            )}
          </div>
        ) : null}

        {sources.length === 0 ? (
          <div style={{ padding: 18, borderTop: "1px solid var(--hair)", fontSize: 12.5, color: "var(--ink3)", lineHeight: 1.6 }}>
            {kind === "private"
              ? "No folders of your own yet. Point this at a checkout and its modules become installable here."
              : "No community modules registered. OCA repositories go here, and they load after your own code."}
          </div>
        ) : (
          sources.map((s) => {
            const count = scan?.modules.filter((m) => m.source_id === s.id).length ?? null;
            return (
              <div className="comp" key={s.id} style={{ gridTemplateColumns: "1fr 110px 100px" }}>
                <div style={{ minWidth: 0 }}>
                  <div style={{ fontSize: 12.5, color: "var(--ink)" }}>{s.label}</div>
                  <div
                    className="m"
                    style={{ fontSize: 11, color: "var(--ink4)", marginTop: 2, overflowWrap: "anywhere" }}
                  >
                    {s.path_or_url}
                  </div>
                </div>
                <span className="badge badge-quiet" style={{ justifySelf: "start" }}>
                  {s.kind.toUpperCase()}
                </span>
                <span className="m" style={{ fontSize: 11.5, color: "var(--ink3)", textAlign: "right" }}>
                  {count === null ? "" : `${count} module${count === 1 ? "" : "s"}`}
                </span>
              </div>
            );
          })
        )}
      </div>

      {collisions.length > 0 ? (
        <div className="card" style={{ overflow: "hidden" }}>
          <div style={{ padding: "14px 18px", display: "flex", alignItems: "center", gap: 10 }}>
            <span style={{ color: "var(--warn)", display: "flex" }}>
              <IconWarn size={14} />
            </span>
            <span className="cap">Same module in two places</span>
          </div>
          <div style={{ padding: "0 18px 14px", fontSize: 11.5, color: "var(--ink3)", lineHeight: 1.6 }}>
            Odoo loads the first copy it finds along the path and silently ignores the rest. These are the ones where
            that's happening:
          </div>
          {collisions.map((c) => (
            <div key={c.technical_name} className="comp" style={{ gridTemplateColumns: "1fr 1fr" }}>
              <span className="m" style={{ fontSize: 12, color: "var(--ink)" }}>
                {c.technical_name}
              </span>
              <span style={{ fontSize: 11.5, color: "var(--ink3)" }}>
                <span style={{ color: "var(--ok)" }}>{c.winner_source_label}</span> wins over{" "}
                {c.shadowed.map((s) => s.source_label).join(", ")}
              </span>
            </div>
          ))}
        </div>
      ) : null}
    </div>
  );
}

// --- Config ----------------------------------------------------------------

function ConfigTab({
  api,
  server,
  databases,
  onError,
  onChanged,
}: {
  api: ScopedApi;
  server: OdooServer;
  databases: Database[];
  onError: (message: string | null) => void;
  onChanged: () => void;
}) {
  const [preview, setPreview] = useState<StartupPreview | null>(null);
  const [saving, setSaving] = useState<null | "python" | "data">(null);
  // Shown immediately on click, then dropped once the reloaded server row
  // says the same thing. Without it the box springs back to its old state
  // for the length of the round trip — and turning on python reloading
  // installs a package, so that round trip can be seconds long. A switch
  // that ignores you for two seconds reads as broken.
  const [pending, setPending] = useState<Partial<ReloadPolicy>>({});
  const policy: ReloadPolicy = { ...server.reload, ...pending };

  // Fetched, not composed here. An approximation written in the front end
  // drifts from the file that actually runs, and a "here's exactly what we
  // ran" panel that's subtly wrong is worse than not having one.
  useEffect(() => {
    let alive = true;
    void api
      .previewOdooConf(server.id)
      .then((p) => alive && setPreview(p))
      .catch(() => alive && setPreview(null));
    return () => {
      alive = false;
    };
  }, [api, server.id]);

  async function setPolicy(patch: { reload_python?: boolean; update_on_data_change?: boolean }) {
    setSaving(patch.reload_python !== undefined ? "python" : "data");
    setPending((p) => ({ ...p, ...patch }));
    onError(null);
    try {
      const updated = await api.setReloadPolicy(server.id, patch);
      setPreview(await api.previewOdooConf(server.id).catch(() => preview));
      onChanged();
      // Reconcile against what the server actually stored, rather than
      // assuming the click won. If the two disagree, the truth wins.
      setPending((p) => {
        const next = { ...p };
        for (const key of Object.keys(patch) as (keyof ReloadPolicy)[]) {
          if (updated.reload[key] === next[key]) delete next[key];
        }
        return next;
      });
    } catch (err) {
      setPending((p) => {
        const next = { ...p };
        for (const key of Object.keys(patch) as (keyof ReloadPolicy)[]) delete next[key];
        return next;
      });
      onError(describeError(err));
    } finally {
      setSaving(null);
    }
  }

  const running = server.state.status === "running";

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
      <div className="card" style={{ padding: "18px 22px" }}>
        <div className="cap" style={{ marginBottom: 12 }}>
          Settings
        </div>
        <Kv label="Odoo version" value={server.odoo_version} mono />
        <Kv label="Port" value={String(server.port)} mono />
        <Kv label="Address pattern" value={`<database>.localhost:${server.port}`} mono />
        <Kv label="Workers" value="0 — threaded, which is what makes breakpoints work" />
        <Kv label="Created" value={timeAgo(server.created_at)} />
      </div>

      {/* --- reload on save ---------------------------------------------------
          Two switches, not one, because they are two different mechanisms
          with different costs and different owners. Collapsing them into a
          single "auto-reload" toggle would hide the fact that one of them
          restarts your process and the other one writes to your database. */}
      <div className="card" style={{ padding: "18px 22px" }}>
        <div className="cap" style={{ marginBottom: 4 }}>
          When code on disk changes
        </div>
        <div style={{ fontSize: 11.5, color: "var(--ink4)", lineHeight: 1.6, marginBottom: 12 }}>
          Off by default. Nothing here happens until you ask for it.
        </div>

        <label className="kv" style={{ cursor: saving ? "wait" : "pointer" }}>
          <span style={{ minWidth: 0 }}>
            <span style={{ fontSize: 12.5 }}>Restart when a .py file changes</span>
            <span style={{ fontSize: 11.5, color: "var(--ink4)", display: "block", marginTop: 2 }}>
              Odoo&rsquo;s own <span className="m">--dev=reload</span>, not ours — it compiles each change before
              acting, so a syntax error lands in the log instead of restarting into a broken server. Turning this on
              installs <span className="m">watchdog</span> into this Odoo&rsquo;s interpreter, because without it Odoo
              logs &ldquo;Code autoreload feature is disabled&rdquo; and quietly does nothing.
            </span>
          </span>
          <input
            type="checkbox"
            checked={policy.reload_python}
            disabled={saving !== null}
            onChange={(e) => void setPolicy({ reload_python: e.target.checked })}
          />
        </label>

        <label className="kv" style={{ cursor: saving ? "wait" : "pointer" }}>
          <span style={{ minWidth: 0 }}>
            <span style={{ fontSize: 12.5 }}>Update the module when its data files change</span>
            <span style={{ fontSize: 11.5, color: "var(--ink4)", display: "block", marginTop: 2 }}>
              This one is ours, because Odoo has no answer for it: a changed view or data XML/CSV only takes effect
              after <span className="m">-u &lt;module&gt;</span>, and nothing in Odoo watches for that. Runs about a
              second after you stop saving, and only on databases where that module is actually installed.
            </span>
          </span>
          <input
            type="checkbox"
            checked={policy.update_on_data_change}
            disabled={saving !== null}
            onChange={(e) => void setPolicy({ update_on_data_change: e.target.checked })}
          />
        </label>

        {policy.reload_python ? (
          <div style={{ display: "flex", gap: 8, alignItems: "flex-start", marginTop: 10, fontSize: 11.5, color: "var(--ink4)", lineHeight: 1.6 }}>
            <IconInfo />
            <span>
              The <span className="m">.py</span> setting becomes a <span className="m">--dev</span> flag on the
              command line, which is built when this Odoo starts
              {running ? (
                <>
                  {" "}
                  — so it applies after the next restart, not to the process running right now.
                </>
              ) : (
                <> — so it applies the next time you start it.</>
              )}{" "}
              {policy.update_on_data_change
                ? "The data-file watcher needs no restart; it's running already."
                : null}
            </span>
          </div>
        ) : null}
      </div>

      <div className="card" style={{ overflow: "hidden" }}>
        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            padding: "14px 18px",
            borderBottom: "1px solid var(--hair)",
          }}
        >
          <span className="cap">What starting this actually runs</span>
          <button
            className="btn btn-q btn-s"
            disabled={!preview}
            onClick={() => preview && void navigator.clipboard?.writeText(`${preview.command}\n\n${preview.conf}`)}
          >
            <IconCopy />
            Copy
          </button>
        </div>
        <pre
          className="m scroll"
          style={{
            margin: 0,
            padding: "14px 18px",
            fontSize: 11.5,
            color: "var(--ink2)",
            lineHeight: 1.7,
            background: "var(--s2)",
          }}
        >
          {preview ? `$ ${preview.command}\n\n${preview.conf}` : "Reading\u2026"}
        </pre>
        <div style={{ padding: "12px 18px", fontSize: 11.5, color: "var(--ink4)", lineHeight: 1.6 }}>
          Rendered by the same code that starts this Odoo, so what you see here is what runs — flags included.{" "}
          <span className="m">dbfilter</span> is what routes <span className="m">name.localhost</span> to the database
          of the same name. Auto-reload is a flag rather than a config line because Odoo reads{" "}
          <span className="m">dev_mode</span> from the file and then overwrites it from{" "}
          <span className="m">--dev</span>.
          {databases.length === 1 ? (
            <>
              {" "}
              One database here ({databases[0].name}), so every address on this port reaches it.
            </>
          ) : null}
        </div>
      </div>

      <UpgradePanel api={api} server={server} databases={databases} onError={onError} onChanged={onChanged} />
    </div>
  );
}

/** Moving a database to a newer major Odoo.
 *
 *  The scariest operation in Odoo, and the one with no tooling: OCA's
 *  OpenUpgrade is the only community path, it can only move one major
 *  version at a time, and nothing wraps the chain — so a three-hop
 *  migration that fails on hop three leaves you between two versions with
 *  whatever you remembered to copy first.
 *
 *  Everything in this panel exists to make that survivable: it runs on a
 *  copy so the original is never at risk, it shows the hops before starting
 *  because the chain is the part people don't expect, and it snapshots
 *  before each hop so any of them can be undone. */
function UpgradePanel({
  api,
  server,
  databases,
  onError,
  onChanged,
}: {
  api: ScopedApi;
  server: OdooServer;
  databases: Database[];
  onError: (m: string | null) => void;
  onChanged: () => void;
}) {
  const [targets, setTargets] = useState<string[]>([]);
  const [to, setTo] = useState("");
  const [source, setSource] = useState(databases[0]?.id ?? "");
  const [plan, setPlan] = useState<UpgradePlan | null>(null);
  const [planError, setPlanError] = useState<string | null>(null);
  const [running, setRunning] = useState(false);
  const [result, setResult] = useState<UpgradeRun | null>(null);

  useEffect(() => {
    void api
      .upgradeTargets()
      .then((found) => {
        // Only versions newer than this Odoo — offering a downgrade would
        // be offering something OpenUpgrade cannot do.
        const newer = found.filter((v) => parseInt(v, 10) > parseInt(server.odoo_version, 10));
        setTargets(newer);
        setTo((current) => current || newer[0] || "");
      })
      .catch(() => setTargets([]));
  }, [api, server.odoo_version]);

  useEffect(() => {
    if (!source && databases[0]) setSource(databases[0].id);
  }, [databases, source]);

  useEffect(() => {
    if (!source || !to) {
      setPlan(null);
      return;
    }
    let alive = true;
    setPlanError(null);
    void api
      .upgradePlan(source, to)
      .then((found) => alive && setPlan(found))
      .catch((err) => {
        if (!alive) return;
        setPlan(null);
        setPlanError(describeError(err));
      });
    return () => {
      alive = false;
    };
  }, [api, source, to]);

  if (databases.length === 0 || targets.length === 0) return null;

  return (
    <div className="card" style={{ padding: "18px 22px" }}>
      <div className="cap" style={{ marginBottom: 4 }}>
        Move to a newer Odoo
      </div>
      <div style={{ fontSize: 11.5, color: "var(--ink4)", lineHeight: 1.6, marginBottom: 14 }}>
        Runs OCA&rsquo;s OpenUpgrade on a <b style={{ fontWeight: 560, color: "var(--ink2)" }}>copy</b>. Whatever
        happens, the database you pick here is not touched.
      </div>

      <div style={{ display: "flex", gap: 10, alignItems: "center", flexWrap: "wrap", marginBottom: 14 }}>
        <select
          value={source}
          onChange={(e) => setSource(e.target.value)}
          disabled={running}
          style={{
            background: "var(--s2)",
            color: "var(--ink2)",
            border: "1px solid var(--hair2)",
            borderRadius: 6,
            padding: "6px 8px",
            fontFamily: "var(--mono)",
            fontSize: 12,
          }}
        >
          {databases.map((d) => (
            <option key={d.id} value={d.id}>
              {d.name}
            </option>
          ))}
        </select>
        <span style={{ fontSize: 12, color: "var(--ink3)" }}>from {server.odoo_version} to</span>
        <select
          value={to}
          onChange={(e) => setTo(e.target.value)}
          disabled={running}
          style={{
            background: "var(--s2)",
            color: "var(--ink2)",
            border: "1px solid var(--hair2)",
            borderRadius: 6,
            padding: "6px 8px",
            fontFamily: "var(--mono)",
            fontSize: 12,
          }}
        >
          {targets.map((v) => (
            <option key={v} value={v}>
              {v}
            </option>
          ))}
        </select>
      </div>

      {planError ? (
        <div style={{ fontSize: 12, color: "var(--warn)", lineHeight: 1.6 }}>{planError}</div>
      ) : null}

      {plan && plan.hops.length > 0 ? (
        <>
          <div style={{ fontSize: 12, color: "var(--ink2)", marginBottom: 8 }}>
            {plan.hops.length === 1 ? (
              <>One step.</>
            ) : (
              <>
                {plan.hops.length} steps — OpenUpgrade can only move one major version at a time, so this is{" "}
                {plan.hops.length} separate migrations in order.
              </>
            )}{" "}
            The copy will be called <span className="m">{plan.into}</span>.
          </div>

          <div
            style={{
              fontSize: 11.5,
              color: "var(--ink4)",
              lineHeight: 1.6,
              marginBottom: 14,
              padding: "8px 10px",
              borderRadius: 7,
              background: "var(--s2)",
            }}
          >
            {plan.carried_addons.length > 0 ? (
              <>
                Your own code rides along too:{" "}
                {plan.carried_addons.map((c) => (
                  <span key={c.label} className="m" style={{ marginRight: 6 }}>
                    {c.label}
                    <span style={{ color: "var(--ink4)" }}>({c.kind})</span>
                  </span>
                ))}
                — without that, a database with a custom module installed can&rsquo;t be migrated at all, Odoo
                couldn&rsquo;t even find the module. But carrying it isn&rsquo;t the same as migrating it:
                OpenUpgrade only ships real migration scripts for Odoo&rsquo;s own modules, so your code just gets
                re-initialized like any addon install. Worth checking it still works on {plan.to} before you trust
                the result.
              </>
            ) : (
              <>No Private or OCA addons are registered on this Odoo, so this migration only touches Odoo&rsquo;s own modules.</>
            )}
          </div>

          {plan.hops.map((hop) => {
            const done = result?.completed.some((c) => c.to === hop.to);
            const failedHere = result?.failed?.hop.to === hop.to;
            return (
              <div
                key={hop.to}
                className="kv"
                style={{ borderColor: failedHere ? "var(--err)" : undefined }}
              >
                <span style={{ minWidth: 0 }}>
                  <span className="m" style={{ fontSize: 12.5, color: failedHere ? "var(--err)" : "var(--ink)" }}>
                    {hop.from} → {hop.to}
                  </span>
                  <span style={{ fontSize: 11.5, color: "var(--ink4)", display: "block", marginTop: 2 }}>
                    {/* Said before starting, because "this will download
                        for ten minutes first" is not a surprise anyone
                        wants mid-migration. */}
                    {hop.odoo_ready && hop.openupgrade_ready
                      ? "everything it needs is already here"
                      : `${[!hop.odoo_ready && `Odoo ${hop.to}`, !hop.openupgrade_ready && "OpenUpgrade"]
                          .filter(Boolean)
                          .join(" and ")} will be downloaded first`}
                  </span>
                </span>
                <span style={{ fontSize: 11.5, color: done ? "var(--ok)" : failedHere ? "var(--err)" : "var(--ink4)" }}>
                  {done ? "done" : failedHere ? "stopped here" : running ? "waiting" : "—"}
                </span>
              </div>
            );
          })}

          {result?.failed ? (
            <div
              style={{
                marginTop: 12,
                padding: "12px 14px",
                borderRadius: 9,
                background: "var(--err-soft, var(--s2))",
                border: "1px solid var(--err)",
                fontSize: 12,
                color: "var(--ink2)",
                lineHeight: 1.6,
              }}
            >
              <b style={{ fontWeight: 560 }}>
                Stopped at {result.failed.hop.from} → {result.failed.hop.to}.
              </b>{" "}
              {result.completed.length > 0
                ? `The ${plural(result.completed.length, "step")} before it worked and still stand. `
                : ""}
              A snapshot was taken just before this step, so the copy can go back to exactly where it was — look for
              it in the Snapshots rail. Your original database was never touched.
              <div
                className="m"
                style={{ fontSize: 11, color: "var(--ink3)", marginTop: 8, maxHeight: 140, overflow: "auto" }}
              >
                {result.failed.message}
              </div>
            </div>
          ) : null}

          {result && !result.failed ? (
            <div style={{ marginTop: 12, fontSize: 12, color: "var(--ok)", lineHeight: 1.6 }}>
              Done. <span className="m">{result.plan.into}</span> is now Odoo {result.plan.to}. It sits on this Odoo
              for now — point a {result.plan.to} server at it when you&rsquo;re ready.
            </div>
          ) : null}

          <button
            className="btn btn-s btn-p"
            style={{ marginTop: 14 }}
            disabled={running || Boolean(result && !result.failed)}
            onClick={() => {
              setRunning(true);
              setResult(null);
              onError(null);
              void api
                .runUpgrade(source, to)
                .then((run) => setResult(run))
                .catch((err) => onError(describeError(err)))
                .finally(() => {
                  setRunning(false);
                  onChanged();
                });
            }}
          >
            {running
              ? `Migrating… (${plural(plan.hops.length, "step")}, this takes a while)`
              : `Migrate a copy to ${plan.to}`}
          </button>
        </>
      ) : null}
    </div>
  );
}

function Kv({ label, value, mono }: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="kv">
      <span style={{ fontSize: 12.5, color: "var(--ink3)" }}>{label}</span>
      <span className={mono ? "m" : undefined} style={{ fontSize: 12.5, color: "var(--ink2)", textAlign: "right" }}>
        {value}
      </span>
    </div>
  );
}

// --- Logs ------------------------------------------------------------------

function LogsTab({
  api,
  events,
  server,
  databases,
  snapshots,
}: {
  api: ScopedApi;
  events: EventEnvelope[];
  server: OdooServer;
  databases: Database[];
  snapshots: Snapshot[];
}) {
  // Two genuinely different things, and conflating them helps nobody:
  // Odoo's own output is where a traceback lives, and the app's record is
  // where "who dropped that database" lives.
  const [view, setView] = useState<"odoo" | "app">("odoo");
  const [lines, setLines] = useState<LogLine[]>([]);
  const [filter, setFilter] = useState("");
  const [errorsOnly, setErrorsOnly] = useState(false);
  const [follow, setFollow] = useState(true);
  const [loaded, setLoaded] = useState(false);
  const paneRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let alive = true;
    api
      .serverLogs(server.id, 2000)
      .then((history) => {
        if (!alive) return;
        setLines(history);
        setLoaded(true);
      })
      .catch(() => alive && setLoaded(true));

    // Live tail, on its own socket — a booting Odoo emits hundreds of
    // lines at once and must not starve the event stream.
    const stop = subscribeLogs((line) => {
      if (line.server_id !== server.id && line.stream !== "notice") return;
      setLines((prev) => (prev.length > 4000 ? [...prev.slice(-3500), line] : [...prev, line]));
    });
    return () => {
      alive = false;
      stop();
    };
  }, [api, server.id]);

  const shown = lines.filter((l) => {
    if (errorsOnly && !/\b(ERROR|CRITICAL|Traceback|WARNING)\b/.test(l.line)) return false;
    if (filter && !l.line.toLowerCase().includes(filter.toLowerCase())) return false;
    return true;
  });

  // Follow the tail unless the reader has scrolled up to look at
  // something — nothing is more annoying than a log that yanks you away
  // from the line you were reading.
  useEffect(() => {
    if (follow && paneRef.current) {
      paneRef.current.scrollTop = paneRef.current.scrollHeight;
    }
  }, [shown.length, follow]);

  const maps: NameMaps = {
    serverNames: new Map([[server.id, server.name]]),
    databaseNames: new Map(databases.map((d) => [d.id, d.name])),
    instanceLabels: new Map(),
    snapshotNames: new Map(snapshots.map((s) => [s.id, s.name])),
  };

  // Consecutive identical entries collapse to one line with a count.
  // Opening a tab that scans the module folders writes an event every
  // time, and fifteen copies of the same sentence buries the one line
  // that actually mattered.
  const rows: { id: string; at: string; text: string; count: number }[] = [];
  for (const e of events) {
    const text = describeEvent(e, maps);
    const last = rows[rows.length - 1];
    if (last && last.text === text) {
      last.count += 1;
    } else {
      rows.push({ id: e.id, at: e.occurred_at, text, count: 1 });
    }
  }

  return (
    <div className="card" style={{ overflow: "hidden" }}>
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: 10,
          padding: "11px 14px",
          borderBottom: "1px solid var(--hair)",
          flexWrap: "wrap",
        }}
      >
        <div className="segtrack" style={{ padding: 3 }}>
          <button className={`seg${view === "odoo" ? " on" : ""}`} style={{ padding: "5px 12px" }} onClick={() => setView("odoo")}>
            Odoo output
          </button>
          <button className={`seg${view === "app" ? " on" : ""}`} style={{ padding: "5px 12px" }} onClick={() => setView("app")}>
            What this app did
          </button>
        </div>

        {view === "odoo" && (
          <>
            <input
              className="field"
              style={{ width: 200, padding: "6px 10px", fontSize: 12 }}
              placeholder="Filter lines…"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
            />
            <button
              className={`btn btn-s${errorsOnly ? " btn-p" : ""}`}
              onClick={() => setErrorsOnly(!errorsOnly)}
              title="Errors, criticals, warnings and tracebacks only"
            >
              Problems only
            </button>
            <label style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 12, color: "var(--ink3)" }}>
              <input type="checkbox" checked={follow} onChange={(e) => setFollow(e.target.checked)} />
              Follow
            </label>
            <div style={{ flex: 1 }} />
            <span className="m" style={{ fontSize: 11, color: "var(--ink4)" }}>
              {shown.length === lines.length ? plural(lines.length, "line") : `${shown.length} of ${lines.length} lines`}
            </span>
          </>
        )}
        {view === "app" && (
          <>
            <div style={{ flex: 1 }} />
            <span style={{ fontSize: 11, color: "var(--ink4)" }}>{plural(events.length, "entry", "entries")}</span>
          </>
        )}
      </div>

      {view === "odoo" ? (
        <>
          <div ref={paneRef} className="scroll" style={{ maxHeight: 460, background: "var(--canvas)" }}>
            {shown.map((l, i) => (
              <div
                key={`${l.at}-${i}`}
                className="m"
                style={{
                  display: "flex",
                  gap: 12,
                  padding: "2px 14px",
                  fontSize: 11.5,
                  lineHeight: 1.55,
                  whiteSpace: "pre-wrap",
                  wordBreak: "break-word",
                  color: lineColour(l),
                  background: l.stream === "notice" ? "var(--warn-soft)" : undefined,
                }}
              >
                <span style={{ color: "var(--ink4)", flexShrink: 0 }}>
                  {new Date(l.at).toLocaleTimeString([], { hour12: false })}
                </span>
                <span>{l.line}</span>
              </div>
            ))}
            {shown.length === 0 && (
              <div style={{ padding: 18, fontSize: 12.5, color: "var(--ink3)", lineHeight: 1.6 }}>
                {!loaded
                  ? "Reading…"
                  : lines.length > 0
                    ? "Nothing matches that filter."
                    : server.state.status === "running"
                      ? "This Odoo is running but hasn't printed anything since this app started watching it — it was started by an earlier run, so its earlier output isn't here."
                      : "Nothing yet. Output appears here from the moment this app starts this Odoo."}
              </div>
            )}
          </div>
          <div style={{ padding: "10px 14px", fontSize: 11, color: "var(--ink4)", lineHeight: 1.6, borderTop: "1px solid var(--hair)" }}>
            Odoo's own output, live. Kept in memory while the app runs — the last 5,000 lines per Odoo — so it isn't
            here after a restart, and it isn't in the event log.
          </div>
        </>
      ) : (
        <>
          {rows.length === 0 ? (
            <div style={{ padding: 18, fontSize: 12.5, color: "var(--ink3)" }}>Nothing recorded for this Odoo yet.</div>
          ) : (
            <div className="scroll" style={{ maxHeight: 460 }}>
              {rows.map((row) => (
                <div
                  key={row.id}
                  style={{
                    display: "flex",
                    gap: 14,
                    padding: "9px 18px",
                    borderBottom: "1px solid var(--hair)",
                    fontSize: 11.5,
                  }}
                >
                  <span className="m" style={{ color: "var(--ink4)", flexShrink: 0 }}>
                    {new Date(row.at).toLocaleTimeString()}
                  </span>
                  <span style={{ color: "var(--ink2)" }}>{row.text}</span>
                  {row.count > 1 ? (
                    <span className="m" style={{ color: "var(--ink4)", flexShrink: 0, marginLeft: "auto" }}>
                      ×{row.count}
                    </span>
                  ) : null}
                </div>
              ))}
            </div>
          )}
          <div style={{ padding: "12px 18px", fontSize: 11.5, color: "var(--ink4)", lineHeight: 1.6 }}>
            This app's own record of every operation — durable, and the thing Activity is built from.
          </div>
        </>
      )}
    </div>
  );
}

/** Odoo's log level, read off the line itself. Colour is a hint here, not
 *  the signal — the level word is right there in the text either way. */
function lineColour(l: LogLine): string {
  if (l.stream === "notice") return "var(--warn)";
  if (/\b(ERROR|CRITICAL)\b/.test(l.line) || l.line.startsWith("Traceback")) return "var(--err)";
  if (/\bWARNING\b/.test(l.line)) return "var(--warn)";
  return "var(--ink2)";
}

// --- Debug -----------------------------------------------------------------

function DebugTab({
  api,
  server,
  databases,
  onError,
  onChanged,
}: {
  api: ScopedApi;
  server: OdooServer;
  databases: Database[];
  onError: (m: string) => void;
  onChanged: () => void;
}) {
  const [target, setTarget] = useState(databases[0]?.id ?? "");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!target && databases[0]) setTarget(databases[0].id);
  }, [databases, target]);

  // The single most common junior-dev confusion in Odoo: --dev=xml only
  // reloads QWeb templates, so every other view change needs a real module
  // update. Making that one button is the whole point of this tab.
  async function applyChanges() {
    if (!target) return;
    setBusy(true);
    try {
      const scan = await api.scanModules(server.id);
      const mine = scan.modules.filter((m) => m.source_kind === "private" && !m.shadowed).map((m) => m.technical_name);
      if (mine.length === 0) {
        onError("No modules of your own are registered on this Odoo, so there's nothing to update.");
        return;
      }
      await api.upgradeModules(target, mine);
    } catch (err) {
      onError(describeError(err));
    } finally {
      setBusy(false);
      onChanged();
    }
  }

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
      <div className="card" style={{ padding: "18px 22px" }}>
        <div className="cap" style={{ marginBottom: 6 }}>
          Apply your code changes
        </div>
        <div style={{ fontSize: 12.5, color: "var(--ink3)", lineHeight: 1.6, marginBottom: 14 }}>
          Python and QWeb templates reload on save. Everything else — form views, fields, security rules, data files —
          needs the module updating before Odoo sees it. That's Odoo's behaviour, not a limitation here.
        </div>
        <div style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap" }}>
          {databases.length > 0 ? (
            <select
              value={target}
              onChange={(e) => setTarget(e.target.value)}
              style={{
                background: "var(--s2)",
                color: "var(--ink2)",
                border: "1px solid var(--hair2)",
                borderRadius: 6,
                padding: "7px 9px",
                fontFamily: "var(--mono)",
                fontSize: 12,
              }}
            >
              {databases.map((d) => (
                <option key={d.id} value={d.id}>
                  {d.name}
                </option>
              ))}
            </select>
          ) : null}
          <button className="btn btn-p" disabled={busy || !target} onClick={() => void applyChanges()}>
            {busy ? "Updating…" : "Apply changes"}
          </button>
          <span style={{ fontSize: 11.5, color: "var(--ink4)" }}>
            updates every module of yours on that database
          </span>
        </div>
      </div>

      <div className="card" style={{ padding: "18px 22px" }}>
        <div className="cap" style={{ marginBottom: 12 }}>
          Development flags in use
        </div>
        <Kv label="dev" value="xml,reload,werkzeug" mono />
        <Kv label="workers" value="0 (threaded — required for breakpoints)" />
        <Kv label="log level" value="info" mono />
        <div style={{ fontSize: 11.5, color: "var(--ink4)", lineHeight: 1.6, paddingTop: 12 }}>
          <span className="m">--dev=reload</span> silently does nothing without the <span className="m">watchdog</span>{" "}
          package installed in the same interpreter, which is a genuinely confusing failure — if saving a Python file
          doesn't restart anything, that's the first thing to check.
        </div>
      </div>
    </div>
  );
}

// --- Editor / Browser ------------------------------------------------------

function NotWiredYet({ which }: { which: "editor" | "browser" }) {
  const copy =
    which === "editor"
      ? {
          title: "The editor isn't wired up yet",
          body: "The plan is openvscode-server running as a supervised sidecar, shown in a panel here, already pointed at this Odoo's code and knowing which database you're hitting.",
          blocker:
            "It needs the desktop window to exist first — a browser tab can't supervise a sidecar process. That's task 0.6, and it needs a real desktop build.",
        }
      : {
          title: "The built-in browser isn't wired up yet",
          body: "The plan is Odoo itself in a panel beside your code, so a change and its effect are on one screen instead of two windows.",
          blocker:
            "Same blocker as the editor: it needs the native window and its webview. Until then, Open in browser on the Databases tab does the same job in your own browser.",
        };

  return (
    <div className="card" style={{ padding: "26px 24px", display: "flex", gap: 14, alignItems: "flex-start" }}>
      <span style={{ color: "var(--ink4)", display: "flex", marginTop: 2 }}>
        <IconInfo size={16} />
      </span>
      <div style={{ maxWidth: 560 }}>
        <div style={{ fontSize: 14, fontWeight: 500, letterSpacing: "-0.013em", marginBottom: 8 }}>{copy.title}</div>
        <div style={{ fontSize: 12.5, color: "var(--ink3)", lineHeight: 1.65, marginBottom: 10 }}>{copy.body}</div>
        <div style={{ fontSize: 12, color: "var(--ink4)", lineHeight: 1.65 }}>{copy.blocker}</div>
      </div>
    </div>
  );
}

// --- Snapshots rail --------------------------------------------------------

function SnapshotRail({
  api,
  server,
  databases,
  snapshots,
  onError,
  onConfirm,
  onChanged,
}: {
  api: ScopedApi;
  server: OdooServer;
  databases: Database[];
  snapshots: Snapshot[];
  onError: (m: string) => void;
  onConfirm: (c: ConfirmSpec) => void;
  onChanged: () => void;
}) {
  const [busy, setBusy] = useState<string | null>(null);
  const [target, setTarget] = useState(databases[0]?.id ?? "");
  /** The branch the linked repo is on right now. Polled rather than read
   *  once: branches are switched in a terminal, not in this app, so the
   *  rail has to notice on its own or it is always one step behind. */
  const [branch, setBranch] = useState<string | null>(null);

  useEffect(() => {
    if (!target && databases[0]) setTarget(databases[0].id);
  }, [databases, target]);

  useEffect(() => {
    let alive = true;
    const read = () =>
      api
        .getGitBranch(server.id)
        .then((r) => alive && setBranch(r.branch))
        .catch(() => alive && setBranch(null));
    void read();
    const timer = window.setInterval(read, 4000);
    return () => {
      alive = false;
      window.clearInterval(timer);
    };
  }, [api, server.id]);

  // Snapshots taken on the branch that is checked out right now, for the
  // database in view. This is what branch binding is *for*: switch to a
  // branch and the database state that belonged to it is one click away,
  // instead of you remembering which of fourteen snapshots went with
  // which piece of work.
  const onThisBranch = branch
    ? snapshots
        .filter((s) => s.git_branch === branch && s.database_id === target)
        .sort((a, b) => b.created_at.localeCompare(a.created_at))
    : [];
  const currentDbSnapshots = snapshots.filter((s) => s.database_id === target);
  // Only worth suggesting when the newest snapshot for this database is
  // *not* the branch's own — otherwise the thing to restore is already
  // the obvious first row and a banner is just noise.
  const suggestion =
    onThisBranch.length > 0 &&
    currentDbSnapshots.sort((a, b) => b.created_at.localeCompare(a.created_at))[0]?.id !== onThisBranch[0].id
      ? onThisBranch[0]
      : null;

  const dbName = (id: string) => databases.find((d) => d.id === id)?.name ?? "a database";
  const unprotected = databases.filter((d) => !snapshots.some((s) => s.database_id === d.id));

  async function take() {
    if (!target) return;
    setBusy("taking");
    try {
      // The label defaults to the branch name for readability; the branch
      // itself is recorded server-side either way, so renaming this later
      // can't break the binding.
      const stamp = new Date().toISOString().slice(5, 16).replace("T", " ");
      await api.createSnapshot(target, branch ?? stamp);
    } catch (err) {
      onError(describeError(err));
    } finally {
      setBusy(null);
      onChanged();
    }
  }

  function askRevert(snapshot: Snapshot) {
    const name = dbName(snapshot.database_id);
    onConfirm({
      // Rung 3: overwrites live data in place, but the counter-snapshot
      // makes it genuinely undoable — which is exactly why the box is
      // pre-checked rather than offered as an afterthought.
      rung: 3,
      title: "Roll this database back?",
      object: name,
      consequence: `everything in it is replaced by the copy taken ${timeAgo(snapshot.created_at)}`,
      reversibility: "Any work done since that copy was taken is overwritten.",
      ripple: "Anyone logged into this database will see the older data on their next click.",
      actionLabel: `Restore ${snapshot.name}`,
      offerCounterSnapshot: true,
      onConfirm: async ({ takeCounterSnapshot }) => {
        onConfirm satisfies unknown;
        setBusy(snapshot.id);
        try {
          await api.revertDatabaseToSnapshot(
            snapshot.database_id,
            snapshot.id,
            takeCounterSnapshot ? `before restoring ${snapshot.name}` : undefined,
          );
        } catch (err) {
          onError(describeError(err));
        } finally {
          setBusy(null);
          onChanged();
        }
      },
    });
  }

  return (
    <div
      className="scroll"
      style={{
        width: 268,
        flexShrink: 0,
        borderLeft: "1px solid var(--hair)",
        padding: "20px 18px 26px",
        display: "flex",
        flexDirection: "column",
        gap: 14,
      }}
    >
      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between" }}>
        <span className="cap">Snapshots</span>
        <button className="btn btn-q btn-s" disabled={!target || busy === "taking"} onClick={() => void take()}>
          {busy === "taking" ? "Taking…" : "Take one"}
        </button>
      </div>

      {branch && (
        <div className="m" style={{ fontSize: 11, color: "var(--ink4)", display: "flex", alignItems: "center", gap: 6 }}>
          <IconBranch size={11} />
          {branch}
        </div>
      )}

      {suggestion && (
        <div
          style={{
            background: "var(--acc-soft)",
            border: "1px solid var(--acc-line)",
            borderRadius: 9,
            padding: "11px 13px",
            fontSize: 11.5,
            color: "var(--ink2)",
            lineHeight: 1.55,
          }}
        >
          You&apos;re on <span className="m">{branch}</span>, and this database was snapshotted on that branch{" "}
          {timeAgo(suggestion.created_at)}.
          <button
            className="btn btn-s btn-p"
            style={{ marginTop: 9, width: "100%" }}
            disabled={busy !== null}
            onClick={() => askRevert(suggestion)}
          >
            Restore {suggestion.name}
          </button>
        </div>
      )}

      {databases.length > 1 ? (
        <select
          value={target}
          onChange={(e) => setTarget(e.target.value)}
          style={{
            background: "var(--s2)",
            color: "var(--ink2)",
            border: "1px solid var(--hair2)",
            borderRadius: 6,
            padding: "6px 8px",
            fontFamily: "var(--mono)",
            fontSize: 11.5,
            width: "100%",
          }}
        >
          {databases.map((d) => (
            <option key={d.id} value={d.id}>
              {d.name}
            </option>
          ))}
        </select>
      ) : null}

      {/* Loss aversion pointed at a real unprotected database — a tension
          the user wants to feel, dischargeable in one click. */}
      {unprotected.length > 0 ? (
        <div
          style={{
            background: "var(--warn-soft)",
            border: "1px solid oklch(0.79 0.13 78 / 0.28)",
            borderRadius: 9,
            padding: "11px 13px",
            fontSize: 11.5,
            color: "var(--ink2)",
            lineHeight: 1.55,
          }}
        >
          {unprotected.length === databases.length
            ? "Nothing here has a snapshot yet."
            : `${plural(unprotected.length, "database")} here have no snapshot: `}
          <span className="m" style={{ color: "var(--warn)" }}>
            {unprotected.map((d) => d.name).join(", ")}
          </span>
        </div>
      ) : databases.length > 0 ? (
        <div style={{ fontSize: 11.5, color: "var(--ok)", display: "flex", alignItems: "center", gap: 7 }}>
          <span className="dot dot-run" />
          all {databases.length} protected
        </div>
      ) : null}

      {snapshots.length === 0 ? (
        <div style={{ fontSize: 11.5, color: "var(--ink4)", lineHeight: 1.6 }}>
          A snapshot is an instant frozen copy. Take one before anything you're not sure about, and rolling back is a
          single click.
        </div>
      ) : (
        <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
          {snapshots
            .slice()
            .sort((a, b) => b.created_at.localeCompare(a.created_at))
            .map((s) => (
              <div
                key={s.id}
                style={{
                  background: "var(--s2)",
                  border: "1px solid var(--hair)",
                  borderRadius: 8,
                  padding: "10px 12px",
                }}
              >
                <div className="m" style={{ fontSize: 12, color: "var(--ink)", overflowWrap: "anywhere" }}>
                  {s.name}
                </div>
                <div className="m" style={{ fontSize: 10.5, color: "var(--ink4)", marginTop: 3 }}>
                  {dbName(s.database_id)} · {timeAgo(s.created_at)}
                </div>
                <div style={{ display: "flex", gap: 6, marginTop: 9 }}>
                  {busy === s.id ? (
                    <span style={{ display: "inline-flex", alignItems: "center", gap: 6, fontSize: 11, color: "var(--ink3)" }}>
                      <span className="dot dot-busy" />
                      <span className="m">restoring</span>
                    </span>
                  ) : (
                    <>
                      <button className="btn btn-s" onClick={() => askRevert(s)}>
                        Restore
                      </button>
                      <button
                        className="btn btn-s btn-q"
                        style={{ color: "var(--ink4)" }}
                        onClick={() =>
                          onConfirm({
                            rung: 2,
                            title: "Delete this snapshot?",
                            object: s.name,
                            consequence: `the frozen copy of ${dbName(s.database_id)} taken ${timeAgo(s.created_at)}`,
                            reversibility:
                              "The live database isn't touched — but this particular point to roll back to is gone.",
                            actionLabel: `Delete ${s.name}`,
                            onConfirm: async () => {
                              setBusy(s.id);
                              try {
                                await api.deleteSnapshot(s.id);
                              } catch (err) {
                                onError(describeError(err));
                              } finally {
                                setBusy(null);
                                onChanged();
                              }
                            },
                          })
                        }
                      >
                        Delete
                      </button>
                    </>
                  )}
                </div>
              </div>
            ))}
        </div>
      )}
    </div>
  );
}
