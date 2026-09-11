import { useEffect, useMemo, useState } from "react";
import type { Database, OdooServer, Project, Snapshot } from "../lib/api";
import type { ScopedApi } from "../lib/scopedApi";
import { ConfirmDialog, type ConfirmSpec } from "../components/Confirm";
import { IconDots, IconFolder, IconPlus } from "../components/Icons";
import { formatBytes, plural, timeAgo } from "../lib/format";

/** The top level: every project as a card.
 *
 *  Cards here, rows everywhere below — deliberately. The design doc's Jakob's-law
 *  argument is that an *instance list* must be a table because that's the
 *  `docker ps` mental model, and cards there would signal consumer product.
 *  Projects aren't instances: they're a handful of named places, closer to a
 *  repo list, and a card is what lets each one carry its own summary at a
 *  glance instead of forcing a click to find out whether anything inside is
 *  unprotected.
 *
 *  The numbers on a card are instruments, not a score: counts of real things
 *  and a protection-coverage fraction that can be discharged in one click.
 *  No points, no badges, no streaks — see the design doc's gamification
 *  section for why those are specifically rejected for this audience. */

interface Props {
  api: ScopedApi;
  onOpen: (projectId: string) => void;
  reloadKey: number;
}

export function Projects({ api, onOpen, reloadKey }: Props) {
  const [projects, setProjects] = useState<Project[] | null>(null);
  const [servers, setServers] = useState<OdooServer[]>([]);
  const [databases, setDatabases] = useState<Database[]>([]);
  const [snapshots, setSnapshots] = useState<Snapshot[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<ConfirmSpec | null>(null);
  const [creating, setCreating] = useState(false);
  const [draftName, setDraftName] = useState("");
  const [renaming, setRenaming] = useState<Project | null>(null);
  const [bump, setBump] = useState(0);

  useEffect(() => {
    let alive = true;
    Promise.all([api.listProjects(), api.listServers(), api.listDatabases(), api.listSnapshots()])
      .then(([p, s, d, snaps]) => {
        if (!alive) return;
        setProjects(p);
        setServers(s);
        setDatabases(d);
        setSnapshots(snaps);
        setError(null);
      })
      .catch((err) => alive && setError(String(err)));
    return () => {
      alive = false;
    };
  }, [api, reloadKey, bump]);

  const stats = useMemo(() => {
    const protectedDbs = new Set(snapshots.map((s) => s.database_id));
    return (projectId: string) => {
      const mine = servers.filter((s) => s.project_id === projectId);
      const ids = new Set(mine.map((s) => s.id));
      const dbs = databases.filter((d) => ids.has(d.server_id));
      return {
        odoos: mine.length,
        running: mine.filter((s) => s.state.status === "running").length,
        databases: dbs.length,
        protected: dbs.filter((d) => protectedDbs.has(d.id)).length,
        bytes: dbs.reduce((sum, d) => sum + d.size_bytes, 0),
        lastTouched: [...mine.map((s) => s.created_at), ...dbs.map((d) => d.created_at)].sort().slice(-1)[0] ?? null,
      };
    };
  }, [servers, databases, snapshots]);

  async function createProject() {
    const name = draftName.trim();
    if (!name) return;
    try {
      const color = (projects?.length ?? 0) % 6;
      const created = await api.createProject(name, color);
      setDraftName("");
      setCreating(false);
      onOpen(created.id);
    } catch (err) {
      setError(String(err));
    }
  }

  async function duplicate(project: Project) {
    try {
      await api.duplicateProject(project.id, `${project.name} (copy)`);
      setBump((n) => n + 1);
    } catch (err) {
      setError(String(err));
    }
  }

  function askDelete(project: Project) {
    const s = stats(project.id);
    if (s.odoos > 0) {
      // Not a dialog: this is a refusal with a reason, because the core
      // genuinely will not cascade. Saying so up front beats letting them
      // confirm something that then 409s.
      setError(
        `“${project.name}” still holds ${plural(s.odoos, "Odoo")}. Open it and remove them first — deleting a project never deletes databases behind your back.`,
      );
      return;
    }
    setConfirm({
      rung: 2,
      title: "Delete this project?",
      object: project.name,
      consequence: "an empty project with nothing filed under it",
      reversibility: "Nothing is stored inside it, so nothing is lost — but the project itself can't be brought back.",
      actionLabel: `Delete ${project.name}`,
      onConfirm: async () => {
        setConfirm(null);
        try {
          await api.deleteProject(project.id);
          setBump((n) => n + 1);
        } catch (err) {
          setError(String(err));
        }
      },
    });
  }

  async function commitRename(project: Project, name: string) {
    setRenaming(null);
    if (!name.trim() || name === project.name) return;
    try {
      await api.updateProject(project.id, name.trim(), project.color);
      setBump((n) => n + 1);
    } catch (err) {
      setError(String(err));
    }
  }

  const totalOdoos = servers.length;
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
        <div style={{ display: "flex", alignItems: "baseline", gap: 12, minWidth: 0 }}>
          <span style={{ fontSize: 17, fontWeight: 600, letterSpacing: "-0.018em" }}>Projects</span>
          <span style={{ fontSize: 12.5, color: "var(--ink4)" }}>
            {projects === null
              ? " "
              : `${plural(projects.length, "project")} · ${plural(totalOdoos, "Odoo")} · ${plural(totalDbs, "database")}`}
          </span>
        </div>
        <button className="btn btn-p" onClick={() => setCreating(true)}>
          <IconPlus />
          New project
        </button>
      </div>

      {error ? <Banner tone="err" onDismiss={() => setError(null)}>{error}</Banner> : null}

      <div className="scroll" style={{ flex: 1, padding: "0 26px 26px" }}>
        {projects === null ? (
          <SkeletonGrid />
        ) : projects.length === 0 && !creating ? (
          <Empty onCreate={() => setCreating(true)} />
        ) : (
          <div
            style={{
              display: "grid",
              gridTemplateColumns: "repeat(auto-fill, minmax(288px, 1fr))",
              gap: 14,
              alignItems: "start",
            }}
          >
            {creating ? (
              <div className="card" style={{ padding: "18px 20px" }}>
                <div className="cap" style={{ marginBottom: 8 }}>
                  New project
                </div>
                <input
                  className="field"
                  autoFocus
                  value={draftName}
                  placeholder="Acme Retail"
                  onChange={(e) => setDraftName(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") void createProject();
                    if (e.key === "Escape") {
                      setCreating(false);
                      setDraftName("");
                    }
                  }}
                />
                <div style={{ display: "flex", justifyContent: "flex-end", gap: 8, marginTop: 12 }}>
                  <button
                    className="btn btn-q btn-s"
                    onClick={() => {
                      setCreating(false);
                      setDraftName("");
                    }}
                  >
                    Cancel
                  </button>
                  <button className="btn btn-p btn-s" disabled={!draftName.trim()} onClick={() => void createProject()}>
                    Create
                  </button>
                </div>
              </div>
            ) : null}

            {projects.map((project) => (
              <ProjectCard
                key={project.id}
                project={project}
                stats={stats(project.id)}
                renaming={renaming?.id === project.id}
                onOpen={() => onOpen(project.id)}
                onRename={() => setRenaming(project)}
                onCommitRename={(name) => void commitRename(project, name)}
                onDuplicate={() => void duplicate(project)}
                onDelete={() => askDelete(project)}
              />
            ))}
          </div>
        )}
      </div>

      {confirm ? <ConfirmDialog spec={confirm} onClose={() => setConfirm(null)} /> : null}
    </div>
  );
}

interface Stats {
  odoos: number;
  running: number;
  databases: number;
  protected: number;
  bytes: number;
  lastTouched: string | null;
}

function ProjectCard({
  project,
  stats,
  renaming,
  onOpen,
  onRename,
  onCommitRename,
  onDuplicate,
  onDelete,
}: {
  project: Project;
  stats: Stats;
  renaming: boolean;
  onOpen: () => void;
  onRename: () => void;
  onCommitRename: (name: string) => void;
  onDuplicate: () => void;
  onDelete: () => void;
}) {
  const [menu, setMenu] = useState(false);
  const [draft, setDraft] = useState(project.name);
  const coverage = stats.databases === 0 ? null : stats.protected / stats.databases;

  return (
    <div
      className={`card card-live sw${project.color % 6}`}
      style={{ padding: "16px 18px 14px", position: "relative", cursor: renaming ? "default" : "pointer" }}
      onClick={() => {
        if (!renaming && !menu) onOpen();
      }}
    >
      <div style={{ display: "flex", alignItems: "flex-start", gap: 12, marginBottom: 14 }}>
        <div className="tile">
          <IconFolder size={17} strokeWidth={1.8} />
        </div>
        <div style={{ flex: 1, minWidth: 0 }}>
          {renaming ? (
            <input
              className="field"
              autoFocus
              style={{ padding: "5px 8px", fontSize: 13, fontFamily: "var(--ui)" }}
              value={draft}
              onClick={(e) => e.stopPropagation()}
              onChange={(e) => setDraft(e.target.value)}
              onBlur={() => onCommitRename(draft)}
              onKeyDown={(e) => {
                if (e.key === "Enter") onCommitRename(draft);
                if (e.key === "Escape") onCommitRename(project.name);
              }}
            />
          ) : (
            <div
              style={{
                fontSize: 14,
                fontWeight: 500,
                letterSpacing: "-0.013em",
                whiteSpace: "nowrap",
                overflow: "hidden",
                textOverflow: "ellipsis",
              }}
            >
              {project.name}
            </div>
          )}
          <div className="m" style={{ fontSize: 11, color: "var(--ink4)", marginTop: 3 }}>
            {stats.odoos === 0
              ? "empty"
              : `${stats.odoos} odoo${stats.odoos === 1 ? "" : "s"} · ${stats.databases} db${stats.databases === 1 ? "" : "s"}${stats.bytes ? ` · ${formatBytes(stats.bytes)}` : ""}`}
          </div>
        </div>
        <div style={{ position: "relative", flexShrink: 0 }}>
          <button
            className="btn btn-q btn-icon"
            aria-label={`Actions for ${project.name}`}
            onClick={(e) => {
              e.stopPropagation();
              setMenu((m) => !m);
            }}
          >
            <IconDots />
          </button>
          {menu ? (
            <>
              <div style={{ position: "fixed", inset: 0, zIndex: 20 }} onClick={(e) => { e.stopPropagation(); setMenu(false); }} />
              <div
                onClick={(e) => e.stopPropagation()}
                style={{
                  position: "absolute",
                  top: "calc(100% + 4px)",
                  right: 0,
                  zIndex: 21,
                  minWidth: 178,
                  background: "var(--s1)",
                  borderRadius: 8,
                  boxShadow: "var(--shadow-pop)",
                  padding: 4,
                  display: "flex",
                  flexDirection: "column",
                  gap: 1,
                }}
              >
                <MenuItem
                  onPick={() => {
                    setMenu(false);
                    onRename();
                  }}
                >
                  Rename
                </MenuItem>
                <MenuItem
                  onPick={() => {
                    setMenu(false);
                    onDuplicate();
                  }}
                  detail="Copies the setup, not the data"
                >
                  Duplicate
                </MenuItem>
                {/* Destructive actions never sit in the primary row — Hick's
                    law applies specifically to destructive menus. */}
                <div style={{ height: 1, background: "var(--hair)", margin: "3px 0" }} />
                <MenuItem
                  danger
                  onPick={() => {
                    setMenu(false);
                    onDelete();
                  }}
                >
                  Delete project
                </MenuItem>
              </div>
            </>
          ) : null}
        </div>
      </div>

      {/* Protection coverage: loss aversion pointed at an unprotected
          database, which is a tension the user wants to feel and can
          discharge in one click. Explicitly chosen over a streak or a
          score — see the design doc. */}
      {coverage === null ? (
        <div style={{ fontSize: 11.5, color: "var(--ink4)", padding: "6px 0 2px" }}>
          {stats.odoos === 0 ? "Nothing in here yet" : "No databases yet"}
        </div>
      ) : (
        <div style={{ padding: "2px 0" }}>
          <div style={{ display: "flex", alignItems: "baseline", justifyContent: "space-between", marginBottom: 6 }}>
            <span className="cap">Protected</span>
            <span
              className="m"
              style={{ fontSize: 11, color: coverage === 1 ? "var(--ok)" : "var(--warn)" }}
            >
              {stats.protected} of {stats.databases}
            </span>
          </div>
          <div className="meter">
            <div
              className={`meter-fill ${coverage === 1 ? "full" : "part"}`}
              style={{ width: `${Math.max(4, Math.round(coverage * 100))}%` }}
            />
          </div>
        </div>
      )}

      <div
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          marginTop: 13,
          paddingTop: 11,
          borderTop: "1px solid var(--hair)",
        }}
      >
        <span style={{ display: "inline-flex", alignItems: "center", gap: 7, fontSize: 11, color: "var(--ink4)" }}>
          {stats.running > 0 ? (
            <>
              <span className="dot dot-run" />
              <span className="m">{stats.running} running</span>
            </>
          ) : stats.odoos > 0 ? (
            <>
              <span className="dot dot-stop" />
              <span className="m">all stopped</span>
            </>
          ) : (
            <span className="m">{timeAgo(project.created_at)}</span>
          )}
        </span>
        <span style={{ fontSize: 11.5, color: "var(--acc-ink)" }}>Open</span>
      </div>
    </div>
  );
}

function MenuItem({
  children,
  detail,
  danger,
  onPick,
}: {
  children: React.ReactNode;
  detail?: string;
  danger?: boolean;
  onPick: () => void;
}) {
  return (
    <button
      onClick={onPick}
      style={{
        display: "block",
        width: "100%",
        textAlign: "left",
        background: "transparent",
        border: "none",
        borderRadius: 5,
        padding: "7px 9px",
        cursor: "pointer",
        color: danger ? "var(--err)" : "var(--ink2)",
        font: "inherit",
        fontSize: 12.5,
      }}
      onMouseEnter={(e) => (e.currentTarget.style.background = "var(--s2)")}
      onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
    >
      {children}
      {detail ? (
        <span style={{ display: "block", fontSize: 10.5, color: "var(--ink4)", marginTop: 2 }}>{detail}</span>
      ) : null}
    </button>
  );
}

export function Banner({
  children,
  tone = "info",
  onDismiss,
}: {
  children: React.ReactNode;
  tone?: "info" | "err" | "warn";
  onDismiss?: () => void;
}) {
  const bg = tone === "err" ? "var(--err-soft)" : tone === "warn" ? "var(--warn-soft)" : "var(--s1)";
  const line = tone === "err" ? "oklch(0.67 0.18 22 / 0.3)" : tone === "warn" ? "oklch(0.79 0.13 78 / 0.3)" : "var(--hair)";
  return (
    <div
      style={{
        margin: "0 26px 14px",
        display: "flex",
        alignItems: "flex-start",
        gap: 11,
        background: bg,
        border: `1px solid ${line}`,
        borderRadius: 9,
        padding: "12px 15px",
      }}
    >
      <div style={{ flex: 1, fontSize: 12.5, color: "var(--ink2)", lineHeight: 1.6 }}>{children}</div>
      {onDismiss ? (
        <button className="btn btn-q btn-s" onClick={onDismiss}>
          Dismiss
        </button>
      ) : null}
    </div>
  );
}

function SkeletonGrid() {
  // 1-3s gets a skeleton matching the real geometry, not a spinner —
  // spinners under a second make an app feel slower.
  return (
    <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(288px, 1fr))", gap: 14 }}>
      {[0, 1, 2].map((i) => (
        <div key={i} className="card" style={{ height: 152, opacity: 0.5 }} />
      ))}
    </div>
  );
}

function Empty({ onCreate }: { onCreate: () => void }) {
  return (
    <div
      className="card"
      style={{
        padding: "44px 32px",
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        textAlign: "center",
        gap: 8,
      }}
    >
      <div style={{ fontSize: 20, fontWeight: 600, letterSpacing: "-0.015em" }}>Let's get Odoo running.</div>
      <div style={{ fontSize: 12.5, color: "var(--ink3)", maxWidth: 420, lineHeight: 1.6 }}>
        A project is just a named place to keep one client's or one experiment's Odoos together. Make one, and the
        next screen walks you through your first Odoo — no Docker, no config files.
      </div>
      <button className="btn btn-p" style={{ marginTop: 10 }} onClick={onCreate}>
        <IconPlus />
        New project
      </button>
    </div>
  );
}
