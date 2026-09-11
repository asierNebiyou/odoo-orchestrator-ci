import { chromium } from "playwright";
import { execFileSync } from "node:child_process";

/** The product thesis, end to end, through the real UI: **my data came
 *  back**.
 *
 *  Everything else this app does is in service of one promise — that you
 *  can try the risky thing on a client's database because you can put it
 *  back. Until this script existed, no browser-level test made that promise
 *  and then checked it: the suites covered the screens, the workspace flow
 *  and the reload switches, but nothing clicked "Restore" and looked in
 *  Postgres to see whether the row was really there again.
 *
 *  So this one writes a real row with `psql`, takes a snapshot by clicking,
 *  destroys the row with `psql`, restores by clicking, and reads the table
 *  back. It replaces `e2e_snapshot.mjs` and `e2e_revert.mjs`, which drove
 *  the deleted Topology and Engine screens and could no longer run at all.
 *
 *  It also covers the friction ladder those scripts never reached: that the
 *  rung-4 drop confirm genuinely refuses until the database name is typed,
 *  and that the rung-3 revert confirm pre-checks its counter-snapshot.
 *
 *  Usage: node e2e_safety_net.mjs <baseUrl> <token> [uiUrl]
 */

const [baseUrl, token, uiUrl = "http://127.0.0.1:4173/"] = process.argv.slice(2);
if (!baseUrl || !token) {
  console.error("usage: node e2e_safety_net.mjs <baseUrl> <token> [uiUrl]");
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
const projectName = `Safety ${run}`;
const odooName = `Net ${run}`;
const dbName = `safety_${run}`;

// --- seed a project, an Odoo and a database through the API ----------------

const project = await api("/projects", { method: "POST", body: JSON.stringify({ name: projectName, color: 5 }) });
// No `postgres_instance_id`: the app is supposed to know its own storage,
// and making a database is what brings the cluster up. Leaving it out here
// is deliberate coverage of that.
const server = await api("/servers", {
  method: "POST",
  body: JSON.stringify({
    name: odooName,
    odoo_version: "17.0",
    port: 8400 + (Date.now() % 300),
    project_id: project.id,
  }),
});
await api(`/servers/${server.id}/databases`, { method: "POST", body: JSON.stringify({ name: dbName }) });

const instance = (await api("/postgres-instances")).find((i) => i.id === server.postgres_instance_id);
if (!instance) {
  console.error("the app did not bring up a Postgres instance for this server");
  process.exit(2);
}

// --- put something in it that a person would recognise ---------------------

// psql against the same cluster the app is driving. A snapshot test that
// only counts rows in this app's own SQLite proves nothing about the data
// the user actually cares about.
const psql = (sql, db = dbName) =>
  execFileSync("psql", ["-U", "postgres", "-h", instance.data_dir, "-p", String(instance.port), "-d", db, "-tAc", sql], {
    encoding: "utf8",
  }).trim();

psql(`CREATE TABLE client_work (note text)`);
psql(`INSERT INTO client_work VALUES ('the invoice logic that took all afternoon')`);
check("there is real data to lose", psql("SELECT note FROM client_work").includes("all afternoon"));

// --- take a snapshot by clicking ------------------------------------------

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
let text = await waitForText(/Snapshots/);

check(
  "reversibility is offered before the risk, not after it",
  /Snapshots/.test(text) && !(await page.locator(".tab").allTextContents()).includes("Snapshots"),
  "the rail sits beside every tab rather than hiding inside one",
);

const before = (await api("/snapshots")).length;
await page.locator("button", { hasText: "Take one" }).click();
await waitForText(/ago/, 60000);
await page.waitForTimeout(2500);
const snapshots = await api("/snapshots");
check("taking a snapshot by clicking really takes one", snapshots.length > before, `${before} → ${snapshots.length}`);

const mine = snapshots.find((s) => s.database_id && s.server_id === server.id);
check("the snapshot knows which database it froze", Boolean(mine), JSON.stringify(mine?.name ?? null));

// --- now destroy the work --------------------------------------------------

psql(`DELETE FROM client_work`);
check("the work is genuinely gone", psql("SELECT count(*) FROM client_work") === "0");

// --- and put it back by clicking ------------------------------------------

// No reload: the snapshot rail is already on screen, and `psql` deleting
// rows behind the app's back is exactly the situation a person is in when
// they reach for Restore.
await page.locator("button", { hasText: /^Restore$/ }).first().click();
text = await waitForText(/Roll this database back\?/);

check("the confirm says what is about to happen to what", /Roll this database back\?/.test(text) && text.includes(dbName));
check("it names what cannot be undone", /Any work done since that copy was taken is overwritten/.test(text));
check("it names who else feels it", /Anyone logged into this database/.test(text));

const counterSnapshot = page.locator("label input[type=checkbox]").first();
check(
  "the counter-snapshot is pre-checked, not offered as an afterthought",
  await counterSnapshot.isChecked(),
  "rung 3 makes the undo the default",
);

const snapshotsBeforeRevert = (await api("/snapshots")).length;
await page.locator("button.btn-danger").click();
await page.waitForTimeout(1200);

// The bug this check exists for: the dialog used to stay up after
// confirming, with its danger button still enabled, because closing it was
// each caller's job and one screen forgot. A guard you can fire twice is
// not a guard.
check(
  "confirming closes the dialog instead of leaving it armed",
  (await page.locator('[role="dialog"]').count()) === 0,
  "the restore is already running behind it",
);

await waitForText(/ago/, 90000);
await page.waitForTimeout(3000);

check("the work came back", psql("SELECT note FROM client_work").includes("all afternoon"), psql("SELECT count(*) FROM client_work") + " rows");
check(
  "and the state before the restore is itself restorable",
  (await api("/snapshots")).length > snapshotsBeforeRevert,
  "the counter-snapshot is a real snapshot, not a promise",
);

// --- a backup you can actually get back ------------------------------------
//
// "Back up" used to write a dump that nothing in this app could ever find
// again: the restore call existed but only somebody who knew the file path
// could reach it. These checks are the reason that gap closed.

await page.locator("button", { hasText: /^Back up$/ }).first().click();
text = await waitForText(/with attachments|rows only/, 90000);
check("a backup taken by clicking shows up where it can be used", /Backups on this machine/.test(text));
check("and it says whether the attachments came too", /with attachments|rows only/.test(text), text.match(/with attachments|rows only/)?.[0] ?? "");

const restoredName = `${dbName}_back`;
await page.locator("button", { hasText: /Restore as/ }).first().click();
await waitForText(/into a new database called/);
check("restoring makes a new database rather than overwriting one", /nothing here overwrites what you have/.test(await bodyText()));

// The safety default. A dump from a client carries their working mail
// server passwords, live crons and real payment credentials.
const neutralizeBox = page.locator('label:has-text("Neutralize it first") input[type=checkbox]');
check("the restore offers to neutralize the copy", (await neutralizeBox.count()) === 1);
check("and it is on unless you turn it off", await neutralizeBox.isChecked(), "the safe copy is the default");
check(
  "it says what neutralizing actually does",
  /mail servers switched off and their passwords\s+wiped|passwords wiped/.test(await bodyText()),
);
await neutralizeBox.uncheck();
check(
  "and turning it off says plainly what that means",
  /can email their customers from this machine/.test(await bodyText()),
  "unticking it has to read as a decision, not a preference",
);
await neutralizeBox.check();

// Scoped to the restore panel: the snapshot rail has "Restore" buttons of
// its own, and a locator that catches those would be testing the wrong
// button entirely.
const nameField = page.locator("input.field.m");
const confirmRestore = nameField.locator("xpath=following-sibling::button[1]");
await nameField.fill(dbName);
check(
  "and it refuses a name that is already taken",
  await confirmRestore.isDisabled(),
  "picking the live database's own name would be the one way to lose data here",
);
check("and says why", /already a database called that here/.test(await bodyText()));

await nameField.fill(restoredName);
await confirmRestore.click();
await waitForText(new RegExp(restoredName), 120000);
await page.waitForTimeout(2000);

const restored = (await api("/databases")).find((d) => d.name === restoredName);
check("the backup really restores into a new database", Boolean(restored), restoredName);
check(
  "and the data is in it",
  psql("SELECT note FROM client_work", restoredName).includes("all afternoon"),
  "restored from the dump, not from the live database",
);
check("the original is untouched", psql("SELECT count(*) FROM client_work") === "1");

// --- the ladder's top rung -------------------------------------------------

await page.locator("nav button", { hasText: /^Databases$/ }).first().click();
await waitForText(new RegExp(dbName));
const dropButtons = page.locator("button", { hasText: /^Drop$/ });
if ((await dropButtons.count()) === 0) {
  // The Databases screen may not offer Drop; the Odoo's own Databases tab does.
  await page.locator("nav button", { hasText: /^Projects$/ }).first().click();
  await waitForText(new RegExp(projectName));
  await page.locator(`text=${projectName}`).first().click();
  await waitForText(new RegExp(odooName));
  await page.locator(`text=${odooName}`).first().click();
  await waitForText(new RegExp(dbName));
}
await page.locator("button", { hasText: /^Drop$/ }).first().click();
text = await waitForText(/Drop this database\?/);

check("dropping asks at the top of the ladder", /Drop this database\?/.test(text));
check(
  "it tells the truth about whether the data can come back",
  /A snapshot of this database exists/.test(text),
  "a snapshot exists by now, and the dialog knows it",
);

const confirmButton = page.locator("button.btn-danger");
check("and it refuses until the name is typed", await confirmButton.isDisabled(), "rung 4 is type-to-confirm");

await page.locator("input.field").fill(dbName.slice(0, -1));
check("a near-miss is still a refusal", await confirmButton.isDisabled(), `typed "${dbName.slice(0, -1)}"`);

await page.locator("input.field").fill(dbName);
check("the exact name unlocks it", !(await confirmButton.isDisabled()));

await page.locator("button", { hasText: "Cancel" }).click();
await page.waitForTimeout(800);
const stillThere = (await api("/databases")).some((d) => d.name === dbName);
check("cancelling really cancels", stillThere, "the database is still there");

// --- and a copy of somebody else's data arrives disarmed -------------------
//
// The dangerous thing this app makes easy is putting a client's production
// dump on a laptop. Odoo ships neutralization for exactly this and the app
// used to ignore it, so a restored copy kept live mail servers (with their
// passwords), live crons and real payment credentials.

// These databases are bare Postgres — no Odoo has initialized them — so the
// honest answer is "nothing to neutralize", not "neutralized" and not
// "live". Reporting either of those would be the lie. (That neutralization
// really disarms a *real* Odoo database is proven against real Odoo 17 in
// `neutralizing_a_restored_copy_really_disarms_it`; this layer's job is the
// wiring and the honesty of the states.)
const states = await api("/neutralization");
check(
  "a database no Odoo has touched reports neither safe nor dangerous",
  states[restored.id] === "not_odoo",
  `got "${states[restored.id]}"`,
);
check(
  "and asking to neutralize it was not treated as a failure",
  badResponses.filter((r) => r.includes("neutralize")).length === 0,
  "a scary error here would teach people to untick the box",
);
check(
  "the restored database records that it came from somewhere else",
  restored.origin === "restored",
  `origin: ${restored.origin} — provenance is what decides whether a copy deserves a warning`,
);
check(
  "while one made here does not",
  (await api("/databases")).find((d) => d.name === dbName).origin === "created",
);

// --- and the log stays useful after the thing it names is gone -------------

// Drop the restored copy for real this time, so there is a dropped-database
// row to read. The copy, not the original — this suite should leave the
// data it made a fuss about protecting.
await api(`/databases/${restored.id}`, { method: "DELETE" });
await page.locator("nav button", { hasText: /^Activity$/ }).first().click();
text = await waitForText(new RegExp(`Dropped database "${restoredName}"`), 30000);

check(
  "a dropped database is still named in the log afterwards",
  text.includes(`Dropped database "${restoredName}"`),
  "the name lives on the event; nothing else remembers it once the row is gone",
);
check(
  "and the log never falls back to a bare id for it",
  !new RegExp(`Dropped database "database [0-9a-f]{8}"`).test(text),
  "which is what it used to say, for the one row people go looking for",
);
check("and the backup row offers to restore itself", text.includes("Restore this backup"));

// The honest negative first: this copy was restored from a dump and never
// backed up itself, so there is no dump to bring it back from — and the row
// correctly offers nothing rather than a button that would fail.
const droppedRow = page.locator("text=" + `Dropped database "${restoredName}"`).first();
check(
  "a dropped database with no backup of its own offers nothing",
  !(await droppedRow.locator("xpath=ancestor::*[3]").innerText()).includes("Restore from its last backup"),
  "an offer that can't complete is worse than none",
);

// Now the case that can complete: something backed up, then dropped.
const spareName = `${dbName}_spare`;
const { database: spare } = await api(`/servers/${server.id}/databases`, { method: "POST", body: JSON.stringify({ name: spareName }) });
await api(`/databases/${spare.id}/backup`, { method: "POST" });
await api(`/databases/${spare.id}`, { method: "DELETE" });

await page.reload({ waitUntil: "networkidle" });
await page.locator("nav button", { hasText: /^Activity$/ }).first().click();
text = await waitForText(new RegExp(`Dropped database "${spareName}"`), 30000);
check(
  "a dropped database that does have a backup offers a real way back",
  text.includes("Restore from its last backup"),
  "the offer appears only when the dump is genuinely still on disk",
);

await page.locator("button", { hasText: "Restore from its last backup" }).first().click();
text = await waitForText(/Bring "/);
check("it says nothing existing is touched, because nothing is", /Nothing existing is touched/.test(text));
check(
  "and it is honest about what the backup misses",
  /isn't in it/.test(text),
  "anything done after the backup was taken",
);

const databasesBefore = (await api("/databases")).length;
await page.locator("button.btn-danger").click();
await page.waitForTimeout(1500);
await waitForText(new RegExp(spareName), 120000);
await page.waitForTimeout(2500);
const back = (await api("/databases")).find((d) => d.name === spareName);
check(
  "restoring from the log really brings it back, under its own name",
  Boolean(back),
  `${databasesBefore} → ${(await api("/databases")).length} databases`,
);

check("no failed requests anywhere in the run", badResponses.length === 0, badResponses.slice(0, 3).join(" | "));
check("no console errors anywhere in the run", consoleErrors.length === 0, consoleErrors.slice(0, 3).join(" | "));

await page.screenshot({ path: "/tmp/safety-net.png", fullPage: true });
await browser.close();

console.log(failures.length === 0 ? "\nALL CHECKS PASSED" : `\n${failures.length} FAILED: ${failures.join(", ")}`);
process.exit(failures.length === 0 ? 0 : 1);
