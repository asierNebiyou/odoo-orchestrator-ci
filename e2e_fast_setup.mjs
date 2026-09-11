import { chromium } from "playwright";

/** The two features that make setup fast, through the real UI.
 *
 *  **Instant databases.** Creating a database used to hand back a bare
 *  Postgres database that no Odoo had touched — not something you could log
 *  into. Now it can arrive ready, and the first one of a shape is kept so
 *  every one after it is a copy. Measured against real Odoo at the `Core`
 *  layer (16.8s cold, 0.21s warm); this layer proves the wiring and, more
 *  importantly, that the screen tells the truth about which of the two is
 *  about to happen.
 *
 *  **Finding addons folders.** Registering module folders by hand, each
 *  with a priority number, is the part of setup people got wrong — and a
 *  wrong order silently changes which copy of a module Odoo loads. Pointing
 *  at a checkout should find the folders and propose the order, with a
 *  stated reason for every guess.
 *
 *  Usage: node e2e_fast_setup.mjs <baseUrl> <token> [uiUrl]
 */

const [baseUrl, token, uiUrl = "http://127.0.0.1:4173/"] = process.argv.slice(2);
if (!baseUrl || !token) {
  console.error("usage: node e2e_fast_setup.mjs <baseUrl> <token> [uiUrl]");
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
  return res.status === 204 ? null : res.json();
}

const run = Date.now().toString(36).slice(-5);
const projectName = `Setup ${run}`;
const odooName = `Fast ${run}`;

const project = await api("/projects", { method: "POST", body: JSON.stringify({ name: projectName, color: 2 }) });
const server = await api("/servers", {
  method: "POST",
  body: JSON.stringify({ name: odooName, odoo_version: "17.0", port: 8500 + (Date.now() % 200), project_id: project.id }),
});

// --- discovery, against the real Odoo checkout on this machine ------------

const checkout = process.env.ORCHESTRATOR_TEST_ODOO_CHECKOUT;
if (checkout) {
  const found = await api(`/addons-discovery?path=${encodeURIComponent(checkout)}`);
  check("pointing at a real Odoo checkout finds its addons folders", found.length >= 2, `${found.length} found`);
  check(
    "including the one holding `base`, which lives somewhere else",
    found.some((r) => r.path.endsWith("odoo/addons")),
    "registering only addons/ is why `base` used to read as 'not on disk'",
  );
  check("and everything in an Odoo checkout is classified as Odoo's own", found.every((r) => r.kind === "core"));
  check("every guess states its reason", found.every((r) => r.why && r.why.length > 5), found[0]?.why ?? "");
  check("and shows what's actually in the folder", found.every((r) => r.module_count > 0 && r.sample.length > 0));
} else {
  console.log("SKIP  discovery against a real checkout — set ORCHESTRATOR_TEST_ODOO_CHECKOUT");
}

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

// --- the create-database choice, and its honesty --------------------------

await page.locator("button", { hasText: "New database" }).click();
let text = await waitForText(/Ready to log into/);

check("creating a database offers a working one, not a bare shell", /Ready to log into/.test(text));
const readyBox = page.locator('label:has-text("Ready to log into") input[type=checkbox]');
check("and that is the default", await readyBox.isChecked(), "a database Odoo hasn't touched isn't one you can log into");
check(
  "the first one is honest about costing a minute",
  /first one runs a real Odoo install/.test(text),
  "rather than promising instant and then booting Odoo",
);

await readyBox.uncheck();
text = await bodyText();
check(
  "and unticking it says exactly what you get instead",
  /empty Postgres database/.test(text) && /nothing to log into yet/.test(text),
);
await readyBox.check();

// --- finding addons folders by clicking ----------------------------------

await page.locator("button", { hasText: "Cancel" }).first().click();
await page.locator(".tab", { hasText: /^My code$/ }).click();
await waitForText(/Your module folders/);
await page.locator("button", { hasText: "Add a folder" }).click();

if (checkout) {
  await page.locator('input[placeholder^="/Users"]').fill(checkout);
  await page.locator("button", { hasText: "Look inside" }).click();
  text = await waitForText(/addons folders? found/, 60000);

  check("looking inside a checkout reports what it found", /addons folders? found/.test(text));
  check(
    "in the order they would load, and says why that matters",
    /Earlier folders win a name collision/.test(text),
  );
  check("each folder shows the reason it was classified that way", /inside an Odoo checkout/.test(text));

  const before = (await api(`/servers/${server.id}/addons-sources`)).length;
  await page.locator("button", { hasText: /^Add \d+ folders?$/ }).click();
  await page.waitForTimeout(2500);
  const after = await api(`/servers/${server.id}/addons-sources`);
  check("and adding them really registers them", after.length > before, `${before} → ${after.length}`);
  check(
    "with the load order preserved",
    after.every((s, i) => i === 0 || after[i - 1].rank <= s.rank),
    after.map((s) => `${s.rank}:${s.label}`).join(" "),
  );
  check(
    "including the folder holding `base`",
    after.some((s) => s.path_or_url.endsWith("odoo/addons")),
  );
} else {
  console.log("SKIP  clicking through discovery — needs ORCHESTRATOR_TEST_ODOO_CHECKOUT");
}

check("no failed requests anywhere in the run", badResponses.length === 0, badResponses.slice(0, 3).join(" | "));
check("no console errors anywhere in the run", consoleErrors.length === 0, consoleErrors.slice(0, 3).join(" | "));

await page.screenshot({ path: "/tmp/fast-setup.png", fullPage: true });
await browser.close();

console.log(failures.length === 0 ? "\nALL CHECKS PASSED" : `\n${failures.length} FAILED: ${failures.join(", ")}`);
process.exit(failures.length === 0 ? 0 : 1);
