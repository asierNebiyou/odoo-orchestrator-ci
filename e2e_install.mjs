import { chromium } from "playwright";

/** The one place browser coverage genuinely shrank when the stale suites
 *  were retired (see the README's "browser suites that were retired"
 *  table): real module installs are proven at the `Core` layer against a
 *  real Odoo 17, but nothing clicked the actual Install button in the UI.
 *
 *  This closes that gap: it registers a real Odoo checkout's addons
 *  folders, opens the Apps tab, clicks Install on a real module that isn't
 *  installed yet, and then asks the *database* — not the screen — whether
 *  it actually happened.
 *
 *  Needs a real Odoo checkout and a provisioned venv, same as
 *  `e2e_fast_setup.mjs` and `e2e_reload_on_save.mjs`:
 *    ORCHESTRATOR_TEST_ODOO_CHECKOUT=/path/to/odoo
 *
 *  Usage: node e2e_install.mjs <baseUrl> <token> [uiUrl]
 */

const [baseUrl, token, uiUrl = "http://127.0.0.1:4173/"] = process.argv.slice(2);
if (!baseUrl || !token) {
  console.error("usage: node e2e_install.mjs <baseUrl> <token> [uiUrl]");
  process.exit(2);
}

const checkout = process.env.ORCHESTRATOR_TEST_ODOO_CHECKOUT;
if (!checkout) {
  console.log("SKIP  e2e_install.mjs needs ORCHESTRATOR_TEST_ODOO_CHECKOUT — nothing to install without a real checkout");
  process.exit(0);
}

const failures = [];
function check(name, ok, detail = "") {
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}${detail ? ` — ${detail}` : ""}`);
  if (!ok) failures.push(name);
}

async function api(path, init = {}) {
  const res = await fetch(`${baseUrl}${path}`, {
    ...init,
    headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json", ...(init.headers ?? {}) },
  });
  if (!res.ok) throw new Error(`${init.method ?? "GET"} ${path} → ${res.status} ${await res.text()}`);
  return res.status === 204 ? null : res.json();
}

const run = Date.now().toString(36).slice(-5);
const projectName = `Install ${run}`;
const odooName = `Installer ${run}`;
const dbName = `installer_db_${run}`;
// `barcodes` (Barcode) depends only on `web`, is never pulled in as a
// dependency of anything in the default "Ready to log into" set, and its
// technical name isn't a prefix of anything install-relevant here except
// `barcodes_gs1_nomenclature` — which the row locator below excludes by
// matching the technical-name line exactly, not by substring.
const MODULE = "barcodes";
const MODULE_LABEL = "Barcode";

const project = await api("/projects", { method: "POST", body: JSON.stringify({ name: projectName, color: 3 }) });
const server = await api("/servers", {
  method: "POST",
  body: JSON.stringify({ name: odooName, odoo_version: "17.0", port: 8700 + (Date.now() % 200), project_id: project.id }),
});
// Same two folders every real-checkout test registers: addons/ and the
// odoo/addons/ that holds base/web/mail — see the address-decision-era
// finding that registering only the first makes core modules read as
// "not on disk".
await api(`/servers/${server.id}/addons-sources`, {
  method: "POST",
  body: JSON.stringify({ label: "core", path_or_url: `${checkout}/addons`, kind: "core", rank: 0 }),
});
await api(`/servers/${server.id}/addons-sources`, {
  method: "POST",
  body: JSON.stringify({ label: "base", path_or_url: `${checkout}/odoo/addons`, kind: "core", rank: 1 }),
});
const { database } = await api(`/servers/${server.id}/databases`, {
  method: "POST",
  body: JSON.stringify({ name: dbName, modules: ["base"] }),
});

const before = await api(`/databases/${database.id}/module-states`);
check(
  `${MODULE} is not installed yet, so this test proves something`,
  !before.some((m) => m.technical_name === MODULE && m.state === "installed"),
  JSON.stringify(before.find((m) => m.technical_name === MODULE)),
);

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
async function waitForText(pattern, timeoutMs = 40000) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    const text = await bodyText();
    if (pattern.test(text)) return text;
    await page.waitForTimeout(300);
  }
  return bodyText();
}

await page.goto(uiUrl, { waitUntil: "networkidle" });
await page.locator("nav button", { hasText: /^Projects$/ }).first().click();
await waitForText(new RegExp(projectName));
await page.locator(`text=${projectName}`).first().click();
await waitForText(new RegExp(odooName));
await page.locator(`text=${odooName}`).first().click();
await waitForText(/Databases on this Odoo/);

await page.locator(".tab", { hasText: /^Apps$/ }).click();
let text = await waitForText(/Search apps|available/i, 20000);

// `note` isn't `application: true` in a stock checkout, so it's hidden
// until "Include Odoo's own modules" is on — the same toggle a person
// installing a dependency-only module would have to find.
await page.locator('label:has-text("Include Odoo\'s own modules") input[type=checkbox]').check();
await page.locator('input[placeholder="Search apps"]').fill(MODULE);
text = await waitForText(new RegExp(MODULE_LABEL), 20000);
check("searching finds the real module from the real checkout", new RegExp(MODULE_LABEL).test(text));

// Matched on the technical-name line, exactly — the display name
// "Barcode" is also a prefix of "Barcode - GS1 Nomenclature", and the
// search box matches technical names too, so both rows can be on screen.
const row = page.locator(".comp").filter({ hasText: new RegExp(`${MODULE} ·`) }).first();
check("it starts out offering Install, not Update/Remove", await row.locator("button", { hasText: "Install" }).isVisible());

await row.locator("button", { hasText: "Install" }).click();
// A real `odoo-bin -i` — this is not a mock, so give it real time.
await page.waitForSelector("text=/working/i", { timeout: 5000 }).catch(() => {});
text = await waitForText(/Remove/, 90000);

check(
  "after installing, the row offers Update/Remove instead of Install",
  await row.locator("button", { hasText: "Remove" }).isVisible(),
);
// Not literally the word "installed": `barcodes`' manifest declares a bare
// `2.0`, and Odoo records it in `ir_module_module` as `17.0.2.0` — so the
// version-comparison staleness heuristic (a real, documented limitation
// named in this same screen's own footer text) flags it stale the instant
// it's installed, and the state column shows "17.0.2.0 → 2.0" instead of
// the word. Both readings agree it's installed; "not installed" is the
// only string that would mean otherwise.
const rowText = await row.textContent();
check(
  "and the state column no longer says not installed",
  !/not installed/i.test(rowText),
  rowText,
);

// The check that actually matters: not the screen's word for it, the
// database's.
const after = await api(`/databases/${database.id}/module-states`);
const installedRow = after.find((m) => m.technical_name === MODULE);
check(
  "ir_module_module really has it installed — not just the DOM",
  installedRow?.state === "installed",
  JSON.stringify(installedRow),
);

check("no failed requests anywhere in the run", badResponses.length === 0, badResponses.slice(0, 3).join(" | "));
check("no console errors anywhere in the run", consoleErrors.length === 0, consoleErrors.slice(0, 3).join(" | "));

await page.screenshot({ path: "/tmp/install.png", fullPage: true });
await browser.close();

console.log(failures.length === 0 ? "\nALL CHECKS PASSED" : `\n${failures.length} FAILED: ${failures.join(", ")}`);
process.exit(failures.length === 0 ? 0 : 1);
