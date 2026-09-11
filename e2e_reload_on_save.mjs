import { chromium } from "playwright";

/** Auto-reload on save, end to end, through the UI.
 *
 *  The thing worth proving here isn't that two checkboxes render — it's
 *  that they are not decorative. Every settings toggle this project has
 *  shipped that *only* wrote a row turned out to be a lie (the auto-snapshot
 *  preference nothing read; `--dev=reload` with no `watchdog` installed).
 *  So these checks follow the setting all the way down: click → API → SQLite
 *  → the actual `odoo.conf` that would be written at start time.
 *
 *  Usage: node e2e_reload_on_save.mjs <baseUrl> <token> [uiUrl]
 */

const [baseUrl, token, uiUrl = "http://127.0.0.1:4173/"] = process.argv.slice(2);
if (!baseUrl || !token) {
  console.error("usage: node e2e_reload_on_save.mjs <baseUrl> <token> [uiUrl]");
  process.exit(2);
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
  return res.json();
}

const run = Date.now().toString(36).slice(-5);
const odooName = `Reload ${run}`;

// --- seed one Odoo through the API ----------------------------------------

const instances = await api("/postgres-instances");
if (instances.length === 0) {
  console.error("no postgres instance to attach an Odoo to");
  process.exit(2);
}
const project = await api("/projects", { method: "POST", body: JSON.stringify({ name: `Reload run ${run}`, color: 3 }) });
const port = 8300 + (Date.now() % 400);
const server = await api("/servers", {
  method: "POST",
  body: JSON.stringify({
    name: odooName,
    odoo_version: "17.0",
    port,
    postgres_instance_id: instances[0].id,
    project_id: project.id,
  }),
});

check("a new Odoo reloads nothing until asked", server.reload.reload_python === false && server.reload.update_on_data_change === false, JSON.stringify(server.reload));

const before = await api(`/servers/${server.id}/odoo-conf`);
const confBefore = before.conf;
check("the previewed config is the real generated one", confBefore.startsWith("[options]\n") && confBefore.includes(`http_port = ${port}`), confBefore.split("\n")[1]);
check(
  "it asks Odoo to route by the first label of the host",
  confBefore.includes("dbfilter = ^%d$"),
  "without this every address on the port shows the database picker",
);
check("with reloading off there is no --dev flag", before.command === "odoo-bin -c odoo.conf", before.command);
check(
  "dev_mode never goes in the file, where Odoo would discard it",
  !confBefore.includes("dev_mode"),
  "Odoo recomputes dev_mode from --dev after reading the config file",
);

// --- now drive the toggles by clicking ------------------------------------

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
await page.locator("nav button", { hasText: /^Projects$/ }).first().click();
await waitForText(new RegExp(`Reload run ${run}`));
await page.locator(`text=Reload run ${run}`).first().click();
await waitForText(new RegExp(odooName));
await page.locator(`text=${odooName}`).first().click();
await waitForText(/Snapshots/);
await page.locator(".tab", { hasText: /^Config$/ }).click();

let text = await waitForText(/When code on disk changes/);
check("the Config tab explains the two mechanisms separately", /Restart when a \.py file changes/.test(text) && /Update the module when its data files change/.test(text));
check("it says whose reloader is whose", /Odoo['’]s own --dev=reload/.test(text) && /Odoo has no answer for it/.test(text));
check("it warns that the setting is nothing without watchdog", /watchdog/.test(text));
check("it starts by saying nothing happens unasked", /Off by default/.test(text));

// The real generated config, fetched — not composed in the browser. Waited
// for rather than read immediately: the panel renders a placeholder until
// the fetch lands, and asserting on the placeholder tests nothing.
text = await waitForText(/odoo-bin -c odoo\.conf/);
check("the config panel shows what the backend would actually run", text.includes("dbfilter = ^%d$") && text.includes(`http_port = ${port}`) && text.includes("odoo-bin -c odoo.conf"));

const boxes = page.locator("label.kv input[type=checkbox]");
check("both switches start off", (await boxes.nth(0).isChecked()) === false && (await boxes.nth(1).isChecked()) === false);

// Data-file updates first: no package install, so it's quick.
await boxes.nth(1).check();
await page.waitForTimeout(1500);
let stored = (await api("/servers")).find((s) => s.id === server.id);
check("turning on data-file updates is really stored", stored.reload.update_on_data_change === true, JSON.stringify(stored.reload));
check("one switch does not flip the other", stored.reload.reload_python === false);

// Python reloading changes the config file the backend would write.
await boxes.nth(0).check();
await waitForText(/--dev=reload,qweb,xml/, 120000);
stored = (await api("/servers")).find((s) => s.id === server.id);
check("turning on python reloading is really stored", stored.reload.reload_python === true, JSON.stringify(stored.reload));

const after = await api(`/servers/${server.id}/odoo-conf`);
check(
  "the setting reaches the real command line, not just a row",
  after.command.endsWith("--dev=reload,qweb,xml"),
  after.command,
);
check("and stays out of the file, which Odoo ignores it in", !after.conf.includes("dev_mode"));
check("and the panel shows that change too", (await bodyText()).includes("--dev=reload,qweb,xml"));
check("it says the .py setting applies on next start, not now", /built when this Odoo starts/.test(await bodyText()));

// Turning it back off must actually remove the key, not leave it blank.
await boxes.nth(0).uncheck();
await page.waitForTimeout(2000);
const off = await api(`/servers/${server.id}/odoo-conf`);
check("turning it back off drops the flag entirely", off.command === "odoo-bin -c odoo.conf", off.command);

check("no failed requests anywhere in the run", badResponses.length === 0, badResponses.slice(0, 3).join(" | "));
check("no console errors anywhere in the run", consoleErrors.length === 0, consoleErrors.slice(0, 3).join(" | "));

await page.screenshot({ path: "/tmp/reload-on-save.png", fullPage: true });
await browser.close();

console.log(failures.length === 0 ? "\nALL CHECKS PASSED" : `\n${failures.length} FAILED: ${failures.join(", ")}`);
process.exit(failures.length === 0 ? 0 : 1);
