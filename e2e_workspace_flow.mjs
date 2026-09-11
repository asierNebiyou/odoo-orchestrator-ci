import { chromium } from "playwright";

/** Replaces `e2e_plugins_activity.mjs`, which asserted a UI that no longer
 *  exists (it drove Topology, Engine and Modules — three screens deleted in
 *  the isolationist rebuild, so every one of its assertions had become
 *  false).
 *
 *  What it keeps from that script, because nothing else covers it: the
 *  whole workspace flow driven **by clicking**, against the real backend.
 *  `e2e_four_screens.mjs` seeds its state through the API; this one makes a
 *  project, an Odoo and a database the way a person does, through the
 *  plugin-registry nav, the next-next wizard, and the row actions — which
 *  is what proves the scoped api genuinely works from inside a screen
 *  rather than only that the screens render.
 *
 *  Usage: node e2e_workspace_flow.mjs <baseUrl> <token> [uiUrl]
 */

const [baseUrl, token, uiUrl = "http://127.0.0.1:4173/"] = process.argv.slice(2);
if (!baseUrl || !token) {
  console.error("usage: node e2e_workspace_flow.mjs <baseUrl> <token> [uiUrl]");
  process.exit(2);
}

const failures = [];
function check(name, ok, detail = "") {
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}${detail ? ` — ${detail}` : ""}`);
  if (!ok) failures.push(name);
}

async function apiCall(path) {
  const res = await fetch(`${baseUrl}${path}`, { headers: { Authorization: `Bearer ${token}` } });
  if (!res.ok) throw new Error(`GET ${path} → ${res.status}`);
  return res.json();
}

// Unique per run, so this can be run repeatedly against a backend that
// keeps its state — which it does, deliberately.
const run = Date.now().toString(36).slice(-5);
const projectName = `Clicked ${run}`;
const odooName = `Shop ${run}`;
const dbName = `clicked_${run}`;

const browser = await chromium.launch({ executablePath: "/opt/pw-browsers/chromium" });
const page = await browser.newPage({ viewport: { width: 1400, height: 950 } });
const consoleErrors = [];
page.on("console", (msg) => msg.type() === "error" && consoleErrors.push(msg.text()));
page.on("pageerror", (err) => consoleErrors.push(String(err)));
const badResponses = [];
page.on("response", (res) => res.status() >= 400 && badResponses.push(`${res.status()} ${res.url()}`));

async function bodyText() {
  return (await page.textContent("body")).replace(/\s+/g, " ").trim();
}

async function waitForText(pattern, timeoutMs = 30000) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    const text = await bodyText();
    if (pattern.test(text)) return text;
    await page.waitForTimeout(300);
  }
  return bodyText();
}

await page.goto(uiUrl, { waitUntil: "networkidle" });

// --- the nav rail is built from the plugin registry, not a switch --------

const navText = (await page.locator("nav").textContent()).replace(/\s+/g, " ").trim();
check(
  "nav rail is rendered from the plugin registry",
  ["Projects", "Databases", "Activity", "Stats", "Settings"].every((l) => navText.includes(l)),
  navText,
);

// --- make a project by clicking -------------------------------------------

await page.locator("nav button", { hasText: /^Projects$/ }).first().click();
await page.waitForTimeout(600);
await page.locator("button", { hasText: "New project" }).first().click();
await page.locator("input[placeholder='Acme Retail']").fill(projectName);
await page.locator("button", { hasText: /^Create$/ }).first().click();
let text = await waitForText(new RegExp(projectName));
check("a project can be created by clicking", text.includes(projectName));

// --- open it and make an Odoo through the next-next wizard ----------------

await page.getByText(projectName, { exact: true }).first().click();
text = await waitForText(/New Odoo/);
check("opening a project shows its own screen", /New Odoo/.test(text));
check("the word 'server' never appears in the project screen", !/\bserver\b/i.test(text), text.match(/\S*server\S*/i)?.[0] ?? "");

await page.locator("button", { hasText: "New Odoo" }).first().click();
await page.waitForTimeout(400);

// Step 1 — what to call it.
await page.locator("input[placeholder='Acme Retail']").fill(odooName);
check("the wizard shows its whole path up front", /Step 1 of 3/.test(await bodyText()));
await page.locator("button", { hasText: /^Next$/ }).click();
await page.waitForTimeout(300);

// Step 2 — which version. The default is taken as-is.
await page.locator("button", { hasText: /^Next$/ }).click();
await page.waitForTimeout(300);

// Step 3 — where the code is. "None yet" is a real answer, and the only
// one that needs no path.
await page.getByText("None yet — just Odoo's own apps").click();
await page.waitForTimeout(200);
await page.locator("button", { hasText: new RegExp(`^Create ${odooName}$`) }).click();

text = await waitForText(new RegExp(odooName), 60000);
check("the wizard really creates an Odoo", text.includes(odooName));

// The wizard makes a database of its own, named after the Odoo — so the
// project should end up with one without anything else being asked.
//
// Polled rather than read once: the Odoo's name appears in the UI as soon
// as the Odoo exists, which is *before* its database has finished being
// created. Reading immediately would be asserting on a race, and would
// fail intermittently in a way that teaches people to re-run the suite
// until it goes green.
// Matched on this run's own id, not just the "shop" prefix — on a backend
// that kept an earlier run's databases, a prefix match would pass on
// somebody else's row and prove nothing.
let fromWizard = [];
for (let i = 0; i < 40 && fromWizard.length === 0; i++) {
  fromWizard = (await apiCall("/databases")).filter(
    (d) => d.name.includes(run) && d.name !== dbName,
  );
  if (fromWizard.length === 0) await page.waitForTimeout(500);
}
check(
  "the wizard makes a database too, without a separate question",
  fromWizard.length > 0,
  fromWizard.map((d) => d.name).join(", "),
);

// --- add a second database from the row action ----------------------------

await page.locator("button", { hasText: "Add a database here" }).first().click();
await page.locator("input[placeholder='database_name']").fill(dbName);
await page.locator("button", { hasText: /^Create$/ }).first().click();
text = await waitForText(new RegExp(dbName), 45000);
check("a database can be added from the row action", text.includes(dbName));

// --- open the one Odoo and take a snapshot from its rail -------------------

await page.getByText(odooName, { exact: true }).first().click();
text = await waitForText(/Snapshots/);
const tabs = await page.locator(".tab").allTextContents();
check(
  "one Odoo shows everything about it as tabs",
  ["Databases", "Apps", "My code", "Community", "Config", "Logs", "Debug", "Editor", "Browser"].every((t) => tabs.includes(t)),
  tabs.join(" · "),
);
check("snapshots sit beside the tabs, not inside one", /Snapshots/.test(text) && !tabs.includes("Snapshots"));

const snapshotsBefore = (await apiCall("/snapshots")).length;
await page.locator("button", { hasText: "Take one" }).click();
await waitForText(/Taking…|ago/, 45000);
await page.waitForTimeout(2500);
const snapshotsAfter = (await apiCall("/snapshots")).length;
check("taking a snapshot really creates one", snapshotsAfter > snapshotsBefore, `${snapshotsBefore} → ${snapshotsAfter}`);

// --- and the read-only screens see all of it ------------------------------

await page.locator("nav button", { hasText: /^Databases$/ }).first().click();
text = await waitForText(new RegExp(dbName));
check("the database made by clicking shows up in Databases", text.includes(dbName) && text.includes(projectName));

await page.locator("nav button", { hasText: /^Activity$/ }).first().click();
text = await waitForText(/Today/);
check(
  "Activity describes what was just done in plain language",
  text.includes(`Created the project "${projectName}"`) && text.includes(`Created database "${dbName}"`),
);
check("Activity never shows raw event-type strings", !/database_created|snapshot_created|project_created/.test(text));
check("no scoreboard language anywhere", !/\b(points|badge|streak|leaderboard|XP)\b/i.test(text));

check("no failed requests anywhere in the run", badResponses.length === 0, badResponses.slice(0, 3).join(" | "));
check("no console errors anywhere in the run", consoleErrors.length === 0, consoleErrors.slice(0, 3).join(" | "));

await page.screenshot({ path: "/tmp/workspace-flow-activity.png", fullPage: true });
await browser.close();

console.log(failures.length === 0 ? "\nALL CHECKS PASSED" : `\n${failures.length} FAILED: ${failures.join(", ")}`);
process.exit(failures.length === 0 ? 0 : 1);
