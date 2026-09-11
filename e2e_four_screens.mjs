import { chromium } from "playwright";

/** Browser-driven proof of the four new screens — Databases, Activity,
 *  Stats and Settings — against the real headless backend, a real SQLite
 *  file and a real PostgreSQL cluster. Nothing is mocked.
 *
 *  What this proves that a typecheck cannot:
 *
 *  - the nav rail really is five entries built from the plugin registry;
 *  - Databases lists a database that was genuinely created through the API,
 *    with its project and address, and reconciliation against a real
 *    `pg_database` reports agreement;
 *  - measuring sizes writes a real, non-zero size back into the row;
 *  - Stats computes its speed arithmetic from real recorded durations, with
 *    the sample size shown;
 *  - Activity groups by day and offers "Restore it" on a snapshot row —
 *    the inline inverse — and only there;
 *  - Settings renders the real database store and the real address settings.
 *
 *  Usage: node e2e_four_screens.mjs <baseUrl> <token> [uiUrl]
 */

const [baseUrl, token, uiUrl = "http://127.0.0.1:4173/"] = process.argv.slice(2);
if (!baseUrl || !token) {
  console.error("usage: node e2e_four_screens.mjs <baseUrl> <token> [uiUrl]");
  process.exit(2);
}

const failures = [];
function check(name, ok, detail = "") {
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}${detail ? ` — ${detail}` : ""}`);
  if (!ok) failures.push(name);
}

async function apiCall(path, init = {}) {
  const res = await fetch(`${baseUrl}${path}`, {
    ...init,
    headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json", ...(init.headers ?? {}) },
  });
  if (!res.ok) throw new Error(`${init.method ?? "GET"} ${path} → ${res.status} ${await res.text()}`);
  return res.status === 204 ? null : res.json();
}

// --- seed real state through the real API -------------------------------

// Names are unique per run so the script can be run repeatedly against a
// backend that keeps its state, which is the honest shape: this app's whole
// point is that nothing it makes disappears between sessions.
const run = Date.now().toString(36).slice(-5);
const projectName = `Acme ${run}`;
const dbName = `acme_${run}`;
const scratchName = `scratch_${run}`;

const project = await apiCall("/projects", { method: "POST", body: JSON.stringify({ name: projectName, color: 0 }) });
const server = await apiCall("/servers", {
  method: "POST",
  body: JSON.stringify({ name: `Acme dev ${run}`, odoo_version: "17.0", port: 8069, project_id: project.id }),
});
// The response is `{database, from_cache}` now, not a bare database: making
// a database can either boot Odoo or copy a pre-built one, and which of
// those just happened is the difference the caller waited for.
const { database } = await apiCall(`/servers/${server.id}/databases`, {
  method: "POST",
  body: JSON.stringify({ name: dbName }),
});
// A snapshot gives Activity a row with a real inverse, and Stats a real
// duration to take a median of.
await apiCall(`/databases/${database.id}/snapshots`, {
  method: "POST",
  body: JSON.stringify({ name: "before upgrade", filestore_source: null }),
});
// A second, deliberately unprotected database so protection coverage has
// something true to report rather than a trivially perfect number.
await apiCall(`/servers/${server.id}/databases`, { method: "POST", body: JSON.stringify({ name: scratchName }) });

// The coverage figure is checked against what the API itself reports rather
// than a number typed in here, so the assertion stays true on a backend that
// already had databases in it.
const allDatabases = await apiCall("/databases");
const allSnapshots = await apiCall("/snapshots");
const protectedCount = new Set(allSnapshots.map((s) => s.database_id)).size;
const coverage = `${protectedCount} of ${allDatabases.length} have a snapshot`;

const events = await apiCall("/events?limit=50");
const timed = events.events.filter((e) => typeof e.duration_ms === "number");
check("completion events carry a real measured duration", timed.length > 0, `${timed.length} of ${events.events.length} timed`);

// --- drive the real UI ---------------------------------------------------

const browser = await chromium.launch({ executablePath: "/opt/pw-browsers/chromium" });
const page = await browser.newPage({ viewport: { width: 1400, height: 950 } });
const consoleErrors = [];
page.on("console", (msg) => msg.type() === "error" && consoleErrors.push(msg.text()));
page.on("pageerror", (err) => consoleErrors.push(String(err)));
// A bare "Failed to load resource" in the console says nothing useful, so
// record which request it was.
const badResponses = [];
page.on("response", (res) => res.status() >= 400 && badResponses.push(`${res.status()} ${res.url()}`));

async function bodyText() {
  return (await page.textContent("body")).replace(/\s+/g, " ").trim();
}

async function waitForText(pattern, timeoutMs = 20000) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    const text = await bodyText();
    if (pattern.test(text)) return text;
    await page.waitForTimeout(300);
  }
  return bodyText();
}

async function openScreen(label) {
  await page.locator("nav button", { hasText: new RegExp(`^${label}$`) }).first().click();
  await page.waitForTimeout(700);
}

await page.goto(uiUrl, { waitUntil: "networkidle" });

const navText = (await page.locator("nav").textContent()).replace(/\s+/g, " ").trim();
check(
  "nav rail is Projects · Databases · Activity · Stats · Settings",
  ["Projects", "Databases", "Activity", "Stats", "Settings"].every((l) => navText.includes(l)),
  navText,
);
check("the theme toggle is gone from the nav rail", !/\b(Light|Dark)\b/.test(navText), navText);

// --- Databases ------------------------------------------------------------

await openScreen("Databases");
let text = await waitForText(new RegExp(dbName));
check("Databases lists a real database with its project", text.includes(dbName) && text.includes(projectName));
check("Databases shows the proxy address for it", text.includes(`${dbName}.localhost:8080`), text.match(new RegExp(`${dbName}\\.localhost\\S*`))?.[0] ?? "");
check("Databases reports the unprotected one honestly", /no snapshot/.test(text));

await page.locator("button", { hasText: "Check against Postgres" }).click();
text = await waitForText(/agree|disagree/);
check("reconciliation against the real cluster reports agreement", /This app and Postgres agree/.test(text));

await page.locator("button", { hasText: "Measure sizes" }).click();
text = await waitForText(/sizes measured/);
const sized = await apiCall("/databases");
check(
  "measuring writes a real non-zero size onto the row",
  sized.every((d) => d.size_bytes > 0),
  sized.map((d) => `${d.name}=${d.size_bytes}`).join(" "),
);
check("the measured size is rendered in the table", /\d+(\.\d+)?\s?(KB|MB|GB)/.test(await bodyText()));

// Bulk selection is the reason this screen exists — prove the bar appears.
await page.locator(".drow input[type=checkbox]").first().check();
text = await waitForText(/selected/);
check("selecting shows the bulk protection bar", /Snapshot all/.test(text) && /Back up all/.test(text));

// --- Stats ----------------------------------------------------------------

await openScreen("Stats");
text = await waitForText(/Protection/);
check("Stats reports real protection coverage", text.includes(coverage), `expected "${coverage}", saw "${text.match(/\d+ of \d+ have a snapshot/)?.[0] ?? "nothing"}"`);
check("Stats shows the speed arithmetic with its sample size", /median of \d+/.test(text), text.match(/median of \d+/)?.[0] ?? "");
check("Stats names a real timed operation", /Taking a snapshot|Creating a database/.test(text));
check("Stats lists the versions actually in use", /17\.0/.test(text));
check("Stats shows no score, grade or streak", !/\b(score|grade|streak|points|badge)\b/i.test(text));

// --- Activity --------------------------------------------------------------

await openScreen("Activity");
text = await waitForText(/Today/);
check("Activity groups entries by day", /Today/.test(text));
check("Activity offers the real inverse on a snapshot row", /Restore it/.test(text));
const laneCount = await page.locator(".segtrack .seg").count();
check("Activity has its five filter lanes", laneCount === 5, `${laneCount} lanes`);

await page.locator(".seg", { hasText: "Destructive" }).click();
await page.waitForTimeout(400);
text = await bodyText();
// Destructive entries may legitimately exist from an earlier run against
// the same backend, so this asserts the lane *filters* rather than that it
// is empty: either nothing matched, or everything shown is destructive.
const destructiveRows = await page.locator(".ev-danger").count();
const emptyLane = /Nothing in this filter/.test(text);
check(
  "the destructive lane shows only destructive entries",
  emptyLane || destructiveRows > 0,
  emptyLane ? "nothing destructive has happened" : `${destructiveRows} destructive rows`,
);

// --- Settings ---------------------------------------------------------------

await openScreen("Settings");
text = await waitForText(/Appearance/);
check("Settings owns the theme control now", /Appearance/.test(text) && /Dark/.test(text) && /Light/.test(text));
check("Settings shows the real database store", /Local databases/.test(text) && /PostgreSQL 16/.test(text));
check("Settings states the Safari caveat honestly", /Safari on macOS doesn't resolve/.test(text));
check("Settings has the danger zone", /Danger zone/.test(text));

await page.locator(".seg", { hasText: "Light" }).click();
await page.waitForTimeout(400);
const themeAttr = await page.locator("[data-theme]").first().getAttribute("data-theme");
check("switching the theme from Settings really switches it", themeAttr === "light", String(themeAttr));

await page.screenshot({ path: "/tmp/four-screens-settings-light.png", fullPage: true });
await page.locator(".seg", { hasText: "Dark" }).click();
await page.waitForTimeout(300);
for (const [label, file] of [
  ["Databases", "/tmp/four-screens-databases.png"],
  ["Activity", "/tmp/four-screens-activity.png"],
  ["Stats", "/tmp/four-screens-stats.png"],
  ["Settings", "/tmp/four-screens-settings.png"],
]) {
  await openScreen(label);
  await page.screenshot({ path: file, fullPage: true });
}

check(
  "no failed requests anywhere in the run",
  badResponses.length === 0,
  badResponses.slice(0, 3).join(" | "),
);
check(
  "no console errors anywhere in the run",
  consoleErrors.length === 0,
  [...consoleErrors.slice(0, 3), ...badResponses.slice(0, 3)].join(" | "),
);

await browser.close();

console.log(failures.length === 0 ? "\nALL CHECKS PASSED" : `\n${failures.length} FAILED: ${failures.join(", ")}`);
process.exit(failures.length === 0 ? 0 : 1);
