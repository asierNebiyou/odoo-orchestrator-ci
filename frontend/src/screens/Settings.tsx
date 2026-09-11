import { useEffect, useState } from "react";
import type { CachedRuntime, CachedTemplate, PostgresInstance } from "../lib/api";
import type { PluginScreenProps } from "../lib/pluginTypes";
import { addressSuffix, formatBytes, plural, proxyPort } from "../lib/format";
import { IconTrash } from "../components/Icons";
import { ConfirmDialog, type ConfirmSpec } from "../components/Confirm";

/** The things that are genuinely settings — every one either already a
 *  real value or a decision the app has to make anyway.
 *
 *  Per-viewer preferences (theme, address suffix, proxy port, defaults for
 *  new Odoos) live in localStorage: they're conveniences belonging to one
 *  person on one machine, not domain state, and every read is guarded so a
 *  private window degrades to the default rather than a broken screen.
 *  Everything else here is real server state. */

const KEYS = {
  suffix: "orchestrator:addressSuffix",
  port: "orchestrator:proxyPort",
  version: "orchestrator:defaultOdooVersion",
  autoSnapshot: "orchestrator:autoSnapshotBeforeDestructive",
};

function read(key: string, fallback: string): string {
  try {
    return localStorage.getItem(key) || fallback;
  } catch {
    return fallback;
  }
}

function write(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Nothing to do — the app works, it just won't remember.
  }
}

export default function Settings({ api, theme, setTheme }: PluginScreenProps) {
  const [instances, setInstances] = useState<PostgresInstance[]>([]);
  const [copies, setCopies] = useState<CachedRuntime[]>([]);
  const [templates, setTemplates] = useState<CachedTemplate[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<ConfirmSpec | null>(null);

  const [suffix, setSuffix] = useState(() => addressSuffix());
  const [port, setPort] = useState(() => String(proxyPort()));
  const [version, setVersion] = useState(() => read(KEYS.version, "17.0"));
  const [autoSnapshot, setAutoSnapshot] = useState(() => read(KEYS.autoSnapshot, "on") === "on");
  const [addressTest, setAddressTest] = useState<string | null>(null);

  async function refresh() {
    try {
      setInstances(await api.listPostgresInstances());
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
    try {
      setCopies(await api.listOdooCopies());
    } catch {
      setCopies([]);
    }
    try {
      setTemplates(await api.listTemplates());
    } catch {
      setTemplates([]);
    }
  }

  useEffect(() => {
    void refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  async function act(label: string, work: () => Promise<unknown>) {
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

  /** Makes the request for real and reports exactly what came back.
   *
   *  Cross-origin means the response body is opaque to this page, so the
   *  honest answer is "something answered" or "nothing answered" — never a
   *  green tick implying more was verified than actually was. */
  async function testAddress() {
    const url = `http://test-address-check.${suffix}${port === "80" ? "" : `:${port}`}/`;
    setAddressTest("Trying…");
    try {
      await fetch(url, { mode: "no-cors" });
      setAddressTest(`Something answered at ${url} — the name resolved and the proxy is reachable.`);
    } catch {
      setAddressTest(
        `Nothing answered at ${url}. Either the proxy isn't on port ${port}, or this browser doesn't resolve *.${suffix} — Safari on macOS doesn't resolve *.localhost subdomains.`,
      );
    }
  }

  const totalCached = copies.reduce((sum, c) => sum + c.size_bytes, 0);
  const templatesTotal = templates.reduce((sum, t) => sum + t.size_bytes, 0);

  return (
    <div className="scroll" style={{ flex: 1, padding: 26, display: "flex", flexDirection: "column", gap: 14, maxWidth: 860 }}>
      <div>
        <div style={{ fontSize: 22, fontWeight: 700, letterSpacing: "-0.015em" }}>Settings</div>
        <div style={{ fontSize: 13, color: "var(--ink3)", marginTop: 3 }}>
          How this app looks, where it puts things, and what it does by default.
        </div>
      </div>

      {error && (
        <div className="card" style={{ padding: 13, background: "var(--err-soft)", borderColor: "var(--err)", color: "var(--err)", fontSize: 12.5 }}>
          {error}
        </div>
      )}

      {busy && (
        <div className="card" style={{ padding: 12, fontSize: 12.5, display: "flex", alignItems: "center", gap: 9 }}>
          <span className="dot dot-busy" />
          {busy}
        </div>
      )}

      {/* --- appearance --------------------------------------------------- */}
      <div className="card set">
        <div className="set-t">Appearance</div>
        <div className="set-d">Remembered on this machine.</div>
        <div className="segtrack" style={{ padding: 3, alignSelf: "flex-start", width: 200 }}>
          {(["dark", "light"] as const).map((value) => (
            <button
              key={value}
              className={`seg${theme === value ? " on" : ""}`}
              style={{ padding: "7px 0" }}
              onClick={() => setTheme?.(value)}
            >
              {value === "dark" ? "Dark" : "Light"}
            </button>
          ))}
        </div>
      </div>

      {/* --- addresses ---------------------------------------------------- */}
      <div className="card set">
        <div className="set-t">Addresses</div>
        <div className="set-d">
          Databases answer on <span className="m">&lt;name&gt;.{suffix}</span> through this app's own proxy.
        </div>

        <div className="kv">
          <span style={{ fontSize: 12.5 }}>Suffix</span>
          <span style={{ display: "flex", gap: 8, alignItems: "center" }}>
            <select
              className="field"
              style={{ width: 150, padding: "7px 10px", fontSize: 12.5 }}
              value={suffix}
              onChange={(e) => {
                setSuffix(e.target.value);
                write(KEYS.suffix, e.target.value);
                setAddressTest(null);
              }}
            >
              <option value="localhost">localhost</option>
              <option value="test">test</option>
            </select>
          </span>
        </div>

        <div className="kv">
          <span style={{ fontSize: 12.5 }}>Proxy port</span>
          <input
            className="field m"
            style={{ width: 110, padding: "7px 10px", fontSize: 12.5 }}
            value={port}
            onChange={(e) => {
              const next = e.target.value.replace(/[^0-9]/g, "");
              setPort(next);
              if (next) write(KEYS.port, next);
              setAddressTest(null);
            }}
          />
        </div>

        <div style={{ display: "flex", gap: 10, alignItems: "center", marginTop: 10 }}>
          <button className="btn btn-s" onClick={() => void testAddress()}>
            Test this address
          </button>
          {addressTest && (
            <span style={{ fontSize: 12, color: "var(--ink3)" }}>{addressTest}</span>
          )}
        </div>

        <div style={{ fontSize: 12, color: "var(--ink4)", marginTop: 12, lineHeight: 1.5 }}>
          <strong style={{ color: "var(--ink3)", fontWeight: 560 }}>Safari on macOS doesn't resolve *.localhost
          subdomains.</strong>{" "}
          Chrome, Edge and Firefox do, with nothing installed. Both <span className="m">localhost</span> and{" "}
          <span className="m">test</span> are reserved by RFC 6761 and can never be sold to anyone — which is why
          neither can break the way <span className="m">.dev</span> did. Using <span className="m">test</span>{" "}
          requires a resolver entry you set up yourself; <span className="m">localhost</span> needs nothing.
        </div>
      </div>

      {/* --- where databases are kept -------------------------------------- */}
      <div className="card set">
        <div className="set-t">Where databases are kept</div>
        <div className="set-d">The real PostgreSQL servers this app runs. Everything you make lives in one of these.</div>

        {instances.length === 0 && (
          <div style={{ fontSize: 12.5, color: "var(--ink4)" }}>
            None yet — one is created for you the first time you make a database.
          </div>
        )}

        {instances.map((instance) => {
          const running = instance.state.status === "running";
          return (
            <div key={instance.id} className="kv">
              <span style={{ minWidth: 0 }}>
                <span style={{ display: "flex", alignItems: "center", gap: 8, fontSize: 12.5 }}>
                  <span className={`dot ${running ? "dot-run" : "dot-stop"}`} />
                  {instance.label}
                  <span className="badge badge-quiet">PostgreSQL {instance.pg_version}</span>
                </span>
                <span
                  className="m"
                  style={{ fontSize: 11, color: "var(--ink4)", display: "block", marginTop: 3, wordBreak: "break-all" }}
                >
                  {instance.data_dir} · port {instance.port}
                </span>
              </span>
              <button
                className="btn btn-s"
                disabled={busy !== null}
                onClick={() =>
                  void act(
                    running ? `Stopping ${instance.label}…` : `Starting ${instance.label}…`,
                    () => (running ? api.stopPostgresInstance(instance.id) : api.startPostgresInstance(instance.id)),
                  )
                }
              >
                {running ? "Stop" : "Start"}
              </button>
            </div>
          );
        })}
      </div>

      {/* --- odoo copies ---------------------------------------------------- */}
      <div className="card set">
        <div className="set-t">Copies of Odoo</div>
        <div className="set-d">
          One copy per version, shared by every project that wants it — {formatBytes(totalCached)} in total. Checkouts
          already on this machine are used where they match and are never deleted from here.
        </div>

        {copies.length === 0 ? (
          <div style={{ fontSize: 12.5, color: "var(--ink4)" }}>
            Nothing downloaded yet. A version is fetched the first time an Odoo needs it and isn't already here.
          </div>
        ) : (
          copies.map((copy) => (
            <div key={copy.folder} className="kv">
              <span style={{ minWidth: 0 }}>
                <span style={{ display: "flex", alignItems: "center", gap: 9, fontSize: 12.5 }}>
                  <span className="m">{copy.version}</span>
                  {!copy.has_venv && <span className="badge badge-quiet">not set up to run</span>}
                  <span className="m" style={{ color: "var(--ink3)", fontSize: 11.5 }}>
                    {formatBytes(copy.size_bytes)}
                  </span>
                </span>
                <span
                  className="m"
                  style={{ fontSize: 11, color: "var(--ink4)", display: "block", marginTop: 3, wordBreak: "break-all" }}
                >
                  {copy.path}
                </span>
              </span>
              <button
                className="btn btn-s"
                disabled={busy !== null}
                onClick={() =>
                  setConfirm({
                    rung: 2,
                    title: `Delete this copy of Odoo ${copy.version}?`,
                    object: copy.path,
                    consequence: `Frees ${formatBytes(copy.size_bytes)}.`,
                    reversibility: "Reversible: it's downloaded again automatically the next time an Odoo needs this version.",
                    ripple: "Any Odoo on this version won't start until it's downloaded again.",
                    actionLabel: `Delete ${copy.version}`,
                    onConfirm: () => void act(`Deleting Odoo ${copy.version}…`, () => api.deleteOdooCopy(copy.folder)),
                  })
                }
              >
                <IconTrash size={12} />
                Delete
              </button>
            </div>
          ))
        )}
      </div>

      {/* --- defaults -------------------------------------------------------- */}
      <div className="card set">
        <div className="set-t">Defaults for new Odoos</div>
        <div className="set-d">What the setup questions start with. Every one can still be changed as you go.</div>

        <div className="kv">
          <span style={{ fontSize: 12.5 }}>Odoo version</span>
          <input
            className="field m"
            style={{ width: 110, padding: "7px 10px", fontSize: 12.5 }}
            value={version}
            onChange={(e) => {
              setVersion(e.target.value);
              write(KEYS.version, e.target.value);
            }}
          />
        </div>

        <label className="kv" style={{ cursor: "pointer" }}>
          <span style={{ minWidth: 0 }}>
            <span style={{ fontSize: 12.5 }}>Take a snapshot before anything destructive</span>
            <span style={{ fontSize: 11.5, color: "var(--ink4)", display: "block", marginTop: 2 }}>
              Pre-checks the safety snapshot on every confirm that offers one. It costs seconds and it's the thing
              that turns "I might lose this" into "I can put it back".
            </span>
          </span>
          <input
            type="checkbox"
            checked={autoSnapshot}
            onChange={(e) => {
              setAutoSnapshot(e.target.checked);
              write(KEYS.autoSnapshot, e.target.checked ? "on" : "off");
            }}
          />
        </label>
      </div>

      {/* --- the template cache ---------------------------------------------- */}
      <div className="card set">
        <div className="set-t">Making databases fast</div>
        <div className="set-d">
          The first database with a given set of apps runs a real Odoo install; the result is kept and every database
          after it is copied from it in about a second. This is that cache.
        </div>

        {templates.length === 0 ? (
          <div style={{ fontSize: 12.5, color: "var(--ink4)" }}>
            Nothing cached yet — the first database you make this way will fill it.
          </div>
        ) : (
          <div className="kv">
            <span style={{ minWidth: 0 }}>
              <span style={{ fontSize: 12.5 }}>
                {plural(templates.length, "pre-built database", "pre-built databases")}
              </span>
              <span style={{ fontSize: 11.5, color: "var(--ink4)", display: "block", marginTop: 2 }}>
                Using {formatBytes(templatesTotal)}. Throwing them away costs only time — each one rebuilds itself the
                next time something needs it.
              </span>
            </span>
            <button
              className="btn btn-s"
              disabled={busy !== null}
              onClick={() =>
                setConfirm({
                  // Rung 2: no data of anyone's is lost, only time.
                  rung: 2,
                  title: "Clear the pre-built databases?",
                  object: `${templates.length} cached`,
                  consequence: `Frees ${formatBytes(templatesTotal)}.`,
                  reversibility:
                    "Nothing is lost. The next database you make rebuilds the cache, which takes a minute that one time.",
                  actionLabel: "Clear the cache",
                  onConfirm: () => void act("Clearing the cache…", () => api.clearTemplates()),
                })
              }
            >
              Clear
            </button>
          </div>
        )}
      </div>

      {/* --- danger zone ------------------------------------------------------ */}
      <div className="card set" style={{ borderColor: "var(--err)" }}>
        <div className="set-t" style={{ color: "var(--err)" }}>
          Danger zone
        </div>
        <div className="set-d">Nothing here touches a database. Both actions are about this app's own files.</div>

        <div className="kv">
          <span style={{ minWidth: 0 }}>
            <span style={{ fontSize: 12.5 }}>Delete every downloaded copy of Odoo</span>
            <span style={{ fontSize: 11.5, color: "var(--ink4)", display: "block", marginTop: 2 }}>
              Frees {formatBytes(totalCached)}. Each version downloads again when something needs it.
            </span>
          </span>
          <button
            className="btn btn-s"
            disabled={copies.length === 0 || busy !== null}
            onClick={() =>
              setConfirm({
                rung: 4,
                title: "Delete every downloaded copy of Odoo?",
                object: "delete every copy",
                consequence: `Deletes ${copies.length} ${copies.length === 1 ? "copy" : "copies"} and frees ${formatBytes(
                  totalCached,
                )}.`,
                reversibility:
                  "Reversible, but slowly: every version downloads again from scratch the next time an Odoo needs it.",
                ripple: "No Odoo will start until its version has been downloaded again. No database is touched.",
                actionLabel: "Delete every copy",
                onConfirm: () =>
                  void act("Deleting downloaded copies…", async () => {
                    for (const copy of copies) {
                      await api.deleteOdooCopy(copy.folder);
                    }
                  }),
              })
            }
          >
            Delete all
          </button>
        </div>
      </div>

      {confirm && <ConfirmDialog spec={confirm} onClose={() => setConfirm(null)} />}
    </div>
  );
}
