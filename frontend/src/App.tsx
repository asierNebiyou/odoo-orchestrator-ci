import { useEffect, useMemo, useState } from "react";
import { PLUGINS } from "./lib/plugins";
import { createScopedApi, createScopedSubscribeEvents } from "./lib/scopedApi";
import { api } from "./lib/api";
import { IconActivity, IconChart, IconDatabase, IconGear, IconGrid, Logo } from "./components/Icons";

/** The shell: a narrow nav rail and one section at a time.
 *
 *  The rail and routing are derived from lib/plugins.ts's registry (task
 *  X.1) rather than a hardcoded union, and each section gets its own scoped
 *  api built from its declared permissions — memoised per section id so
 *  switching doesn't churn the object on every render. */

const ICONS: Record<string, (p: { size?: number }) => JSX.Element> = {
  workspace: IconGrid,
  databases: IconDatabase,
  activity: IconActivity,
  stats: IconChart,
  settings: IconGear,
};

// Dark is the default; the choice is remembered per viewer. localStorage is
// exactly the right place for it — a per-viewer convenience, not domain
// state — and every access is guarded because a private window or blocked
// site data must degrade to "dark", never to a broken screen.
const THEME_KEY = "orchestrator:theme";

function readTheme(): "dark" | "light" {
  try {
    return localStorage.getItem(THEME_KEY) === "light" ? "light" : "dark";
  } catch {
    return "dark";
  }
}

export default function App() {
  const [theme, setTheme] = useState<"dark" | "light">(readTheme);
  const [screenId, setScreenId] = useState(PLUGINS[0].id);
  const [storageStatus, setStorageStatus] = useState<{ label: string; running: boolean } | null>(null);

  const active = PLUGINS.find((p) => p.id === screenId) ?? PLUGINS[0];
  const scopedApi = useMemo(() => createScopedApi(active.permissions), [active.id]);
  const scopedSubscribeEvents = useMemo(() => createScopedSubscribeEvents(active.permissions), [active.id]);

  useEffect(() => {
    try {
      localStorage.setItem(THEME_KEY, theme);
    } catch {
      // Nothing to do — the app works fine, it just won't remember.
    }
  }, [theme]);

  // The rail's footer answers "is the thing underneath everything actually
  // up?" without making anyone open a screen to find out.
  useEffect(() => {
    let alive = true;
    const check = () =>
      api
        .listPostgresInstances()
        .then((instances) => {
          if (!alive) return;
          const running = instances.find((i) => i.state.status === "running");
          setStorageStatus(
            instances.length === 0
              ? { label: "no databases set up", running: false }
              : running
                ? { label: "databases ready", running: true }
                : { label: "databases stopped", running: false },
          );
        })
        .catch(() => alive && setStorageStatus({ label: "not connected", running: false }));
    void check();
    const timer = window.setInterval(check, 5000);
    return () => {
      alive = false;
      window.clearInterval(timer);
    };
  }, []);

  return (
    <div
      data-theme={theme}
      style={{
        display: "flex",
        height: "100vh",
        background: "var(--canvas)",
        color: "var(--ink)",
        fontFamily: "var(--ui)",
        fontSize: 13,
        letterSpacing: "-0.008em",
      }}
    >
      <nav
        style={{
          width: 198,
          flexShrink: 0,
          borderRight: "1px solid var(--hair)",
          background: "var(--s1)",
          display: "flex",
          flexDirection: "column",
          padding: "16px 12px",
        }}
      >
        <div style={{ display: "flex", alignItems: "center", gap: 9, padding: "0 6px 18px" }}>
          <Logo />
          <span style={{ fontWeight: 600, fontSize: 13.5, letterSpacing: "-0.015em" }}>Orchestrator</span>
        </div>

        <div style={{ display: "flex", flexDirection: "column", gap: 2 }}>
          {PLUGINS.map((plugin) => {
            const Icon = ICONS[plugin.id] ?? IconGrid;
            return (
              <button
                key={plugin.id}
                className={`nav${plugin.id === active.id ? " on" : ""}`}
                onClick={() => setScreenId(plugin.id)}
              >
                <Icon size={14} />
                <span style={{ flex: 1 }}>{plugin.navLabel}</span>
              </button>
            );
          })}
        </div>

        <div style={{ flex: 1 }} />

        {/* The theme switch used to sit here. It isn't navigation, so it
            moved to Settings — the rail is now only places to go. */}

        <div
          style={{
            borderTop: "1px solid var(--hair)",
            marginTop: 8,
            paddingTop: 12,
            paddingLeft: 6,
            display: "flex",
            alignItems: "center",
            gap: 8,
          }}
        >
          <span className={`dot ${storageStatus?.running ? "dot-run" : "dot-stop"}`} style={{ width: 6, height: 6 }} />
          <span className="m" style={{ fontSize: 10.5, color: "var(--ink4)" }}>
            {storageStatus?.label ?? "checking…"}
          </span>
        </div>
      </nav>

      <main style={{ flex: 1, display: "flex", minWidth: 0 }}>
        <active.component
          api={scopedApi}
          subscribeEvents={scopedSubscribeEvents}
          navigate={setScreenId}
          theme={theme}
          setTheme={setTheme}
        />
      </main>
    </div>
  );
}
