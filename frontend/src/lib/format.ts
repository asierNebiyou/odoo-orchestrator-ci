/** Formatting for machine-generated values. Everything here produces a
 *  string meant to be rendered in monospace (`className="m"`), because the
 *  design system reserves proportional type for prose. */

export function formatBytes(bytes: number): string {
  if (!bytes) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  const value = bytes / Math.pow(1024, i);
  return `${value >= 100 || i === 0 ? Math.round(value) : value.toFixed(1)} ${units[i]}`;
}

/** "12m ago", "3d ago" — never a bare timestamp in a row, because the
 *  question a row answers is "how stale is this", not "what time was it". */
export function timeAgo(iso: string | null): string {
  if (!iso) return "never";
  const then = new Date(iso).getTime();
  if (Number.isNaN(then)) return "never";
  const secs = Math.max(0, Math.floor((Date.now() - then) / 1000));
  if (secs < 45) return "just now";
  if (secs < 3600) return `${Math.floor(secs / 60)}m ago`;
  if (secs < 86400) return `${Math.floor(secs / 3600)}h ago`;
  if (secs < 2592000) return `${Math.floor(secs / 86400)}d ago`;
  return `${Math.floor(secs / 2592000)}mo ago`;
}

/** Uptime from `started_at`, which the core clears on every stop/crash —
 *  so this never reports uptime for a process that isn't running. */
export function uptime(startedAt: string | null): string {
  if (!startedAt) return "";
  const secs = Math.max(0, Math.floor((Date.now() - new Date(startedAt).getTime()) / 1000));
  if (secs < 60) return `${secs}s`;
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins}m`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ${mins % 60}m`;
  return `${Math.floor(hours / 24)}d ${hours % 24}h`;
}

/** The address a database answers on.
 *
 *  `<name>.localhost` (or a custom domain) goes through this app's own
 *  proxy, which is what makes one Odoo serving several databases and one
 *  Odoo per database look identical from the outside.
 *
 *  `.localhost` rather than `.odoo` on purpose: it's reserved by RFC 6761
 *  and modern browsers resolve `*.localhost` to loopback with no setup at
 *  all, whereas `.odoo` is an undelegated TLD — and ICANN's new-gTLD
 *  window opened in April 2026, which is exactly how `.dev` stopped
 *  working for everyone who'd used it locally. */
export function databaseUrl(database: { name: string; domain?: string | null }, proxyPort: number | null): string {
  const host = database.domain ?? `${database.name}.${addressSuffix()}`;
  return proxyPort && proxyPort !== 80 ? `${host}:${proxyPort}` : host;
}

/** The suffix default addresses use. `localhost` needs nothing installed
 *  on any platform; `test` is the other TLD reserved by RFC 6761, for
 *  anyone who has set up a resolver for it.
 *
 *  This is a real setting rather than a constant because the proxy matches
 *  a default address on its **first label** — so `acme.localhost` and
 *  `acme.test` reach the same database with no server-side change. (A
 *  database given a custom domain is matched exactly and is unaffected;
 *  see `find_route` in orchestrator-api/src/proxy.rs.) */
export function addressSuffix(): string {
  try {
    return localStorage.getItem("orchestrator:addressSuffix") || "localhost";
  } catch {
    return "localhost";
  }
}

/** Where this machine's proxy listens. 8080 until the one-time admin
 *  step that lets it have :80; remembered per viewer because it's a
 *  property of this machine's setup, not of any project. */
export function proxyPort(): number {
  try {
    const raw = localStorage.getItem("orchestrator:proxyPort");
    const n = raw ? Number(raw) : NaN;
    return Number.isFinite(n) && n > 0 ? n : 8080;
  } catch {
    return 8080;
  }
}

export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`;
}

/** Whether the safety-snapshot box in a destructive confirm starts
 *  checked. On by default: the design principles put "counter-snapshot
 *  offered, pre-checked" at the heart of the friction ladder, because it
 *  converts the fear into a solved, visible fact at the moment the fear
 *  occurs.
 *
 *  This is what the Settings toggle actually controls. It changes the
 *  *default*, never the availability — the box is still there to untick,
 *  so turning this off is a preference, not a way to hide the safety net. */
export function autoSnapshotBeforeDestructive(): boolean {
  try {
    return localStorage.getItem("orchestrator:autoSnapshotBeforeDestructive") !== "off";
  } catch {
    return true;
  }
}
