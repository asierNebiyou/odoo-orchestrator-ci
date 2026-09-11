import { useEffect, useMemo, useState } from "react";
import type { Database, DiskUsage, EventEnvelope, ModuleScan, OdooServer, ProcessUsage, Project, Snapshot } from "../lib/api";
import type { PluginScreenProps } from "../lib/pluginTypes";
import { formatBytes, plural, timeAgo, uptime } from "../lib/format";
import { IconShield } from "../components/Icons";

/** Instruments, and only ones that are true.
 *
 *  Every number here is measured: disk from real directory sizes and real
 *  `pg_database_size`, speed from real recorded durations, protection from
 *  real snapshot rows. Nothing is estimated, and anything not measured
 *  today is absent rather than drawn and quietly faked. Memory and CPU
 *  per process were absent for exactly that reason until they were
 *  instrumented properly; they're read from the OS now, and an Odoo this
 *  app isn't supervising still shows nothing rather than a zero.
 *
 *  No score, no grade, no ranking. The design principles reject points,
 *  badges and streaks explicitly; what replaces them is arithmetic
 *  someone can act on. */

interface Timing {
  kind: string;
  label: string;
  medianMs: number;
  samples: number;
  slowestMs: number;
}

const TIMED_KINDS: [string, string][] = [
  ["database_created", "Creating a database"],
  ["database_duplicated", "Duplicating a database"],
  ["snapshot_created", "Taking a snapshot"],
  ["database_reverted_to_snapshot", "Restoring a snapshot"],
  ["database_backed_up", "Backing up"],
  ["database_restored", "Restoring a backup"],
];

export default function Stats({ api, subscribeEvents }: PluginScreenProps) {
  const [projects, setProjects] = useState<Project[]>([]);
  const [servers, setServers] = useState<OdooServer[]>([]);
  const [databases, setDatabases] = useState<Database[]>([]);
  const [snapshots, setSnapshots] = useState<Snapshot[]>([]);
  const [events, setEvents] = useState<EventEnvelope[]>([]);
  const [disk, setDisk] = useState<DiskUsage | null>(null);
  const [scans, setScans] = useState<ModuleScan[]>([]);
  /** Per version: where a copy of it actually is, if anywhere. Asked for
   *  rather than assumed — "found on this machine" is a claim, and one
   *  the app has a real way to check. */
  const [whereVersions, setWhereVersions] = useState<Map<string, string | null>>(new Map());
  /** Real memory and CPU per running Odoo, measured from the OS. Empty
   *  until measured, and a server missing from it isn't running. */
  const [usage, setUsage] = useState<Record<string, ProcessUsage>>({});
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  async function refresh() {
    try {
      const [prj, srv, dbs, snaps, evts] = await Promise.all([
        api.listProjects(),
        api.listServers(),
        api.listDatabases(),
        api.listSnapshots(),
        api.recentEvents(500),
      ]);
      setProjects(prj);
      setServers(srv);
      setDatabases(dbs);
      setSnapshots(snaps);
      setEvents(evts);
      setError(null);

      // Ask, per distinct version, whether a usable copy is actually
      // here — cached, adopted from a checkout, or nowhere yet. This is
      // a lookup, not a download.
      const found = new Map<string, string | null>();
      for (const version of new Set(srv.map((s) => s.odoo_version))) {
        try {
          const answer = await api.findOdoo(version);
          found.set(version, answer.found ? answer.source : null);
        } catch {
          found.set(version, null);
        }
      }
      setWhereVersions(found);
      // Disk is its own call and its own failure: a store that can't be
      // measured shouldn't blank the rest of the page.
      try {
        setDisk(await api.diskUsage());
      } catch {
        setDisk(null);
      }
      try {
        setUsage(await api.processUsage());
      } catch {
        setUsage({});
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  useEffect(() => {
    void refresh();
    return subscribeEvents(() => void refresh());
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Module health is one scan per Odoo, and a scan walks the filesystem —
  // so it's explicitly asked for rather than run on every page load.
  async function scanEverything() {
    setBusy("Checking modules across every Odoo…");
    const results: ModuleScan[] = [];
    for (const server of servers) {
      try {
        results.push(await api.scanModules(server.id));
      } catch {
        // An Odoo with no addons folders registered yet simply has
        // nothing to report — not an error worth a banner.
      }
    }
    setScans(results);
    setBusy(null);
  }

  const protectedIds = useMemo(() => new Set(snapshots.map((s) => s.database_id)), [snapshots]);
  const unprotected = databases.filter((d) => !protectedIds.has(d.id));
  const oldestUnprotected = unprotected
    .slice()
    .sort((a, b) => a.created_at.localeCompare(b.created_at))[0];

  const running = servers.filter((s) => s.state.status === "running");

  const timings = useMemo(() => computeTimings(events), [events]);

  const versions = useMemo(() => {
    const counts = new Map<string, number>();
    for (const server of servers) {
      counts.set(server.odoo_version, (counts.get(server.odoo_version) ?? 0) + 1);
    }
    return [...counts.entries()].sort((a, b) => b[1] - a[1]);
  }, [servers]);

  const moduleHealth = useMemo(() => {
    if (scans.length === 0) return null;
    const names = new Set<string>();
    let shadowed = 0;
    let unresolved = 0;
    for (const scan of scans) {
      for (const m of scan.modules) {
        names.add(m.technical_name);
        if (m.shadowed) shadowed += 1;
      }
      unresolved += scan.unresolved_depends.length;
    }
    return { total: names.size, shadowed, unresolved };
  }, [scans]);

  const diskSegments = disk
    ? [
        { label: "Databases", bytes: disk.database_bytes, color: "var(--acc)" },
        { label: "Copies of Odoo", bytes: disk.odoo_copy_bytes, color: "var(--run)" },
        { label: "Snapshot filestores", bytes: disk.snapshot_filestore_bytes, color: "var(--ok)" },
        { label: "Backups", bytes: disk.backup_bytes, color: "var(--warn)" },
        { label: "Module folders", bytes: disk.module_bytes, color: "var(--ink4)" },
      ].filter((s) => s.bytes > 0)
    : [];
  const diskTotal = diskSegments.reduce((sum, s) => sum + s.bytes, 0);

  return (
    <div className="scroll" style={{ flex: 1, padding: 26, display: "flex", flexDirection: "column", gap: 14 }}>
      <div>
        <div style={{ fontSize: 22, fontWeight: 700, letterSpacing: "-0.015em" }}>Stats</div>
        <div style={{ fontSize: 13, color: "var(--ink3)", marginTop: 3 }}>
          Measured facts about this machine — {plural(projects.length, "project")}, {plural(servers.length, "Odoo")},{" "}
          {plural(databases.length, "database")}.
        </div>
      </div>

      {error && (
        <div className="card" style={{ padding: 13, background: "var(--err-soft)", borderColor: "var(--err)", color: "var(--err)", fontSize: 12.5 }}>
          {error}
        </div>
      )}

      {/* --- protection ------------------------------------------------- */}
      <div className="card" style={{ padding: 18, display: "flex", alignItems: "flex-start", gap: 14 }}>
        <div
          style={{
            width: 34,
            height: 34,
            borderRadius: 8,
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            flexShrink: 0,
            color: unprotected.length > 0 ? "var(--warn)" : "var(--ok)",
            background: unprotected.length > 0 ? "var(--warn-soft)" : "var(--ok-soft)",
          }}
        >
          <IconShield size={17} />
        </div>
        <div style={{ flex: 1 }}>
          <div className="cap">Protection</div>
          <div className="stat-value" style={{ marginTop: 2 }}>
            {databases.length === 0
              ? "No databases yet"
              : `${databases.length - unprotected.length} of ${databases.length} have a snapshot`}
          </div>
          <div style={{ fontSize: 12.5, color: "var(--ink3)", marginTop: 4 }}>
            {databases.length === 0
              ? "Nothing to protect yet."
              : unprotected.length === 0
                ? "Every database has something to fall back to."
                : `Longest unprotected: ${oldestUnprotected.name}, made ${timeAgo(oldestUnprotected.created_at)}.`}
          </div>
        </div>
      </div>

      {/* --- disk -------------------------------------------------------- */}
      <div className="card" style={{ padding: 18 }}>
        <div style={{ display: "flex", alignItems: "baseline", gap: 10, marginBottom: 12 }}>
          <span className="cap">Where the disk went</span>
          <span className="m" style={{ fontSize: 12, color: "var(--ink3)" }}>
            {disk ? formatBytes(diskTotal) : "—"}
          </span>
        </div>

        {!disk && <div style={{ fontSize: 12.5, color: "var(--ink4)" }}>Couldn't measure disk usage right now.</div>}

        {disk && diskTotal === 0 && (
          <div style={{ fontSize: 12.5, color: "var(--ink4)" }}>
            Nothing measurable yet. Database sizes are measured on the Databases screen.
          </div>
        )}

        {disk && diskTotal > 0 && (
          <>
            <div className="bar">
              {diskSegments.map((seg) => (
                <span
                  key={seg.label}
                  title={`${seg.label}: ${formatBytes(seg.bytes)}`}
                  style={{ width: `${(seg.bytes / diskTotal) * 100}%`, background: seg.color }}
                />
              ))}
            </div>
            <div style={{ display: "flex", flexWrap: "wrap", gap: "6px 20px", marginTop: 12 }}>
              {diskSegments.map((seg) => (
                <span key={seg.label} style={{ display: "flex", alignItems: "center", gap: 7, fontSize: 12.5 }}>
                  <span style={{ width: 8, height: 8, borderRadius: 2, background: seg.color }} />
                  <span style={{ color: "var(--ink2)" }}>{seg.label}</span>
                  <span className="m" style={{ color: "var(--ink3)", fontSize: 11.5 }}>
                    {formatBytes(seg.bytes)}
                  </span>
                </span>
              ))}
            </div>

            {disk.odoo_copies.length > 0 && (
              <div style={{ marginTop: 14, borderTop: "1px solid var(--hair)", paddingTop: 12 }}>
                <div className="cap" style={{ marginBottom: 6 }}>
                  Copies of Odoo this app downloaded
                </div>
                {disk.odoo_copies.map((copy) => (
                  <div key={copy.folder} className="kv">
                    <span style={{ display: "flex", alignItems: "center", gap: 9 }}>
                      <span className="m" style={{ fontSize: 12.5 }}>
                        {copy.version}
                      </span>
                      {!copy.has_venv && <span className="badge badge-quiet">not set up to run</span>}
                    </span>
                    <span className="m" style={{ fontSize: 11.5, color: "var(--ink3)" }}>
                      {formatBytes(copy.size_bytes)}
                    </span>
                  </div>
                ))}
                <div style={{ fontSize: 12, color: "var(--ink4)", marginTop: 8 }}>
                  Copies are deleted from Settings, so it can't happen by accident from a stats page.
                </div>
              </div>
            )}
          </>
        )}
      </div>

      {/* --- speed ------------------------------------------------------- */}
      <div className="card" style={{ padding: 18 }}>
        <div className="cap" style={{ marginBottom: 4 }}>
          How long things actually take here
        </div>
        <div style={{ fontSize: 12.5, color: "var(--ink3)", marginBottom: 14 }}>
          Medians from what this machine really did — not estimates. Each carries its own sample size, because a
          median of two is not a fact.
        </div>

        {timings.length === 0 ? (
          <div style={{ fontSize: 12.5, color: "var(--ink4)" }}>
            Nothing timed yet. These fill in as you make databases, take snapshots and restore them.
          </div>
        ) : (
          <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(180px, 1fr))", gap: 18 }}>
            {timings.map((t) => (
              <div key={t.kind} className="fact">
                <span style={{ fontSize: 12.5, color: "var(--ink2)" }}>{t.label}</span>
                <span className="fact-n">{formatDuration(t.medianMs)}</span>
                <span className="m" style={{ fontSize: 11, color: "var(--ink4)" }}>
                  median of {t.samples} · slowest {formatDuration(t.slowestMs)}
                </span>
              </div>
            ))}
          </div>
        )}
      </div>

      {/* --- running + versions ------------------------------------------ */}
      <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(280px, 1fr))", gap: 14 }}>
        <div className="card" style={{ padding: 18 }}>
          <div className="cap" style={{ marginBottom: 10 }}>
            Running now
          </div>
          {running.length === 0 ? (
            <div style={{ fontSize: 12.5, color: "var(--ink4)" }}>Nothing is running.</div>
          ) : (
            running.map((server) => {
              const used = usage[server.id];
              return (
                <div key={server.id} className="kv">
                  <span style={{ display: "flex", alignItems: "center", gap: 8, fontSize: 12.5, minWidth: 0 }}>
                    <span className="dot dot-run" />
                    <span style={{ overflow: "hidden", textOverflow: "ellipsis" }}>{server.name}</span>
                  </span>
                  <span className="m" style={{ fontSize: 11.5, color: "var(--ink3)", display: "flex", gap: 10, flexShrink: 0 }}>
                    <span>up {uptime(server.started_at)}</span>
                    {/* Measured, or absent. A process this app started in
                        an earlier run of itself has no supervisor here to
                        measure, and saying nothing is the honest answer. */}
                    {used && <span>{formatBytes(used.memory_bytes)}</span>}
                    {used && <span>{used.cpu_percent.toFixed(1)}% cpu</span>}
                  </span>
                </div>
              );
            })
          )}
          {running.length > 0 && Object.keys(usage).length === 0 && (
            <div style={{ fontSize: 11.5, color: "var(--ink4)", marginTop: 8 }}>
              Memory and CPU aren't available for these — they were started by an earlier run of this app, so nothing
              here is supervising them to measure.
            </div>
          )}
        </div>

        <div className="card" style={{ padding: 18 }}>
          <div className="cap" style={{ marginBottom: 10 }}>
            Versions in use
          </div>
          {versions.length === 0 ? (
            <div style={{ fontSize: 12.5, color: "var(--ink4)" }}>No Odoos yet.</div>
          ) : (
            versions.map(([version, count]) => {
              const cached = disk?.odoo_copies.find((c) => c.version === version);
              const source = whereVersions.get(version);
              // Three genuinely different states, and the third is the one
              // worth saying out loud: an Odoo on a version that isn't here
              // yet won't start until it's been fetched.
              const where = cached
                ? `downloaded · ${formatBytes(cached.size_bytes)}`
                : source === "adopted"
                  ? "a checkout already on this machine"
                  : source
                    ? "on this machine"
                    : "not on this machine yet";
              return (
                <div key={version} className="kv">
                  <span className="m" style={{ fontSize: 12.5 }}>
                    {version}
                  </span>
                  <span style={{ display: "flex", alignItems: "center", gap: 9 }}>
                    <span style={{ fontSize: 12, color: "var(--ink3)" }}>{plural(count, "Odoo")}</span>
                    <span className="badge badge-quiet">{where}</span>
                  </span>
                </div>
              );
            })
          )}
        </div>
      </div>

      {/* --- module health ------------------------------------------------ */}
      <div className="card" style={{ padding: 18 }}>
        <div style={{ display: "flex", alignItems: "center", gap: 12, marginBottom: moduleHealth ? 12 : 0 }}>
          <span className="cap">Modules across everything</span>
          <div style={{ flex: 1 }} />
          <button className="btn btn-s" onClick={() => void scanEverything()} disabled={busy !== null || servers.length === 0}>
            {busy ? "Checking…" : scans.length > 0 ? "Check again" : "Check now"}
          </button>
        </div>

        {!moduleHealth ? (
          <div style={{ fontSize: 12.5, color: "var(--ink4)" }}>
            Checking reads every registered module folder from disk, so it runs when you ask rather than on every
            visit.
          </div>
        ) : (
          <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(160px, 1fr))", gap: 18 }}>
            <div className="fact">
              <span style={{ fontSize: 12.5, color: "var(--ink2)" }}>Distinct modules</span>
              <span className="fact-n">{moduleHealth.total}</span>
            </div>
            <div className="fact">
              <span style={{ fontSize: 12.5, color: "var(--ink2)" }}>In more than one folder</span>
              <span className="fact-n" style={{ color: moduleHealth.shadowed > 0 ? "var(--warn)" : undefined }}>
                {moduleHealth.shadowed}
              </span>
              <span style={{ fontSize: 11, color: "var(--ink4)" }}>the folder order decides which one wins</span>
            </div>
            <div className="fact">
              <span style={{ fontSize: 12.5, color: "var(--ink2)" }}>Missing dependencies</span>
              <span className="fact-n" style={{ color: moduleHealth.unresolved > 0 ? "var(--err)" : undefined }}>
                {moduleHealth.unresolved}
              </span>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

/** Medians of real recorded durations, per kind of operation.
 *
 *  Only events that carry a `duration_ms` count — older rows recorded
 *  before durations were measured are skipped rather than treated as
 *  instant, which would drag every median toward a flattering lie. */
function computeTimings(events: EventEnvelope[]): Timing[] {
  const out: Timing[] = [];
  for (const [kind, label] of TIMED_KINDS) {
    const samples = events
      .filter((e) => e.type === kind && typeof e.duration_ms === "number")
      .map((e) => e.duration_ms as number)
      .sort((a, b) => a - b);
    if (samples.length === 0) continue;
    out.push({
      kind,
      label,
      medianMs: samples[Math.floor(samples.length / 2)],
      samples: samples.length,
      slowestMs: samples[samples.length - 1],
    });
  }
  return out;
}

function formatDuration(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(ms < 10_000 ? 1 : 0)}s`;
  const mins = Math.floor(ms / 60_000);
  return `${mins}m ${Math.round((ms % 60_000) / 1000)}s`;
}
