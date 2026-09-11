import { chromium } from "playwright";

/** Major-version migration, through the real UI.
 *
 *  The actual migration is proven against real Odoo in
 *  `a_real_database_really_migrates_a_major_version` — a real 16.0 database
 *  taken to 17.0 by real OpenUpgrade, with the data intact and the original
 *  untouched. That takes minutes and two Odoo checkouts, so it does not
 *  belong in a browser suite.
 *
 *  What this checks is the part a person actually meets: that the screen
 *  tells the truth *before* they commit. The chain is the thing nobody
 *  expects — OpenUpgrade cannot skip versions, so 15 → 18 is three separate
 *  migrations, each of which can fail on its own — and a UI that hides that
 *  behind one "Upgrade" button is how somebody ends up stranded between two
 *  versions on a Friday afternoon.
 *
 *  Usage: node e2e_migration.mjs <baseUrl> <token> [uiUrl]
 */

const [baseUrl, token, uiUrl = "http://127.0.0.1:4173/"] = process.argv.slice(2);
if (!baseUrl || !token) {
  console.error("usage: node e2e_migration.mjs <baseUrl> <token> [uiUrl]");
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
  const body = res.status === 204 ? null : await res.json().catch(() => null);
  return { status: res.status, body };
}

async function ok(path, init) {
  const { status, body } = await api(path, init);
  if (status >= 400) throw new Error(`${init?.method ?? "GET"} ${path} → ${status} ${JSON.stringify(body)}`);
  return body;
}

const run = Date.now().toString(36).slice(-5);
const projectName = `Migrate ${run}`;
const oldOdoo = `Legacy ${run}`;
const dbName = `legacy_${run}`;

// An Odoo on an old version, which is what makes a migration meaningful.
const project = await ok("/projects", { method: "POST", body: JSON.stringify({ name: projectName, color: 4 }) });
const server = await ok("/servers", {
  method: "POST",
  body: JSON.stringify({ name: oldOdoo, odoo_version: "15.0", port: 8600 + (Date.now() % 200), project_id: project.id }),
});
const { database } = await ok(`/servers/${server.id}/databases`, {
  method: "POST",
  body: JSON.stringify({ name: dbName }),
});

// --- the plan, before any UI ----------------------------------------------

const plan = await ok(`/databases/${database.id}/upgrade-plan?to=18.0`);
check(
  "15 → 18 is planned as three hops, not one",
  plan.hops.length === 3,
  plan.hops.map((h) => `${h.from}→${h.to}`).join(", "),
);
check("each hop knows where it starts", plan.hops[0].from === "15.0" && plan.hops[1].from === "16.0");
check(
  "and the plan names the copy it would work on",
  plan.into === `${dbName}_v180`,
  "the original is never the thing being migrated",
);

const downgrade = await api(`/databases/${database.id}/upgrade-plan?to=14.0`);
check(
  "a downgrade is refused with a reason, not a 500",
  downgrade.status === 422 && /no OpenUpgrade path/.test(downgrade.body?.error ?? ""),
  `${downgrade.status} ${downgrade.body?.error ?? ""}`,
);

// --- and what a person is actually shown ----------------------------------

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
await waitForText(new RegExp(oldOdoo));
await page.locator(`text=${oldOdoo}`).first().click();
await waitForText(/Snapshots/);
await page.locator(".tab", { hasText: /^Config$/ }).click();

let text = await waitForText(/Move to a newer Odoo/);
check("an old Odoo is offered a way forward", /Move to a newer Odoo/.test(text));
check(
  "and it leads with the fact that it works on a copy",
  /Runs OCA.s OpenUpgrade on a copy/.test(text) && /is not touched/.test(text),
  "this is the sentence that makes the operation survivable",
);

// The version selects: source database, then target version.
const selects = page.locator(".card select");
const targets = await selects.last().locator("option").allTextContents();
check("only newer versions are offered", targets.every((v) => parseInt(v, 10) > 15), targets.join(", "));
check("a downgrade is not in the list at all", !targets.includes("14.0"));

await selects.last().selectOption("18.0");
// Waited for something only the *new* plan says. The default target is the
// newest version, so "separate migrations in order" was already on screen
// before this click — waiting for that matched the old plan and tested
// nothing.
text = await waitForText(new RegExp(`${dbName}_v180`));
check(
  "picking a distant version says plainly that it is several migrations",
  /3 steps/.test(text) && /one major version at a time/.test(text),
  "the chain is the part nobody expects",
);
check("every hop is listed by name", /15\.0 → 16\.0/.test(text) && /17\.0 → 18\.0/.test(text));
check(
  "and each says what it still has to download",
  /will be downloaded first/.test(text),
  "learned before starting, not during",
);
check("the copy's name is shown before committing", text.includes(`${dbName}_v180`));

await selects.last().selectOption("16.0");
text = await waitForText(new RegExp(`${dbName}_v160`));
check("and a single-version move says so instead", /One step/.test(text) && text.includes(`${dbName}_v160`));

check(
  "the button says what it will do to what",
  await page.locator("button", { hasText: /Migrate a copy to 16\.0/ }).isVisible(),
);

check("no failed requests anywhere in the run", badResponses.length === 0, badResponses.slice(0, 3).join(" | "));
check("no console errors anywhere in the run", consoleErrors.length === 0, consoleErrors.slice(0, 3).join(" | "));

await page.screenshot({ path: "/tmp/migration.png", fullPage: true });
await browser.close();

console.log(failures.length === 0 ? "\nALL CHECKS PASSED" : `\n${failures.length} FAILED: ${failures.join(", ")}`);
process.exit(failures.length === 0 ? 0 : 1);
