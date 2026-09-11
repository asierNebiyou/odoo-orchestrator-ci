// Real (if narrow) unit proof that task X.1's permission scopes are
// *enforced*, not just declared — runs the actual TypeScript source
// (createScopedApi/createScopedSubscribeEvents) via tsx, not a
// reimplementation of the guard logic. Network is stubbed with a fake
// `fetch` since the point here is proving the scope check runs before any
// request is made, not re-testing orchestrator-api itself (that's what the
// curl/Playwright E2E scripts against the real running binary are for).
//
// Run: cd frontend && npx vite-node ../scopedapi_test.mjs
// (vite-node, not plain tsx — api.ts reads import.meta.env, which only
// exists under Vite's module runner; tsx alone throws on that read.)

globalThis.fetch = async (url) => {
  fetchCalls.push(String(url));
  return { ok: true, status: 200, json: async () => ({ ok: true }) };
};
const fetchCalls = [];

const { createScopedApi, createScopedSubscribeEvents, ScopeDeniedError } = await import(
  "./frontend/src/lib/scopedApi.ts"
);

const checks = [];
function check(desc, cond) {
  checks.push([desc, !!cond]);
}

// --- a plugin with only read scopes cannot call a write method ---------
const readOnly = createScopedApi(["databases:read", "snapshots:read"]);
let threw = null;
try {
  await readOnly.dropDatabase("some-id");
} catch (err) {
  threw = err;
}
check("dropDatabase without databases:write throws ScopeDeniedError", threw instanceof ScopeDeniedError);
check("the thrown error names the missing scope", threw?.scope === "databases:write");
check("no network call was made for the denied call", fetchCalls.length === 0);

// --- the same plugin CAN call a method its granted scope covers --------
fetchCalls.length = 0;
await readOnly.listDatabases();
check("listDatabases with databases:read succeeds", fetchCalls.length === 1);

// --- a method needing two scopes is denied if only one is granted ------
const halfPrivileged = createScopedApi(["databases:write"]); // missing snapshots:write
threw = null;
try {
  await halfPrivileged.revertDatabaseToSnapshot("db-id", "snap-id");
} catch (err) {
  threw = err;
}
check(
  "revertDatabaseToSnapshot with only databases:write (missing snapshots:write) is denied",
  threw instanceof ScopeDeniedError && threw.scope === "snapshots:write",
);

// --- and succeeds once both scopes are granted --------------------------
fetchCalls.length = 0;
const fullyPrivileged = createScopedApi(["databases:write", "snapshots:write"]);
await fullyPrivileged.revertDatabaseToSnapshot("db-id", "snap-id");
check("revertDatabaseToSnapshot succeeds once both required scopes are granted", fetchCalls.length === 1);

// --- subscribeEvents is gated by events:read too ------------------------
const noEvents = createScopedApi([]);
threw = null;
try {
  createScopedSubscribeEvents([])(() => {});
} catch (err) {
  threw = err;
}
check("subscribeEvents without events:read throws ScopeDeniedError", threw instanceof ScopeDeniedError);

// --- an Activity-shaped plugin (all reads) never trips a write check ----
const activityScopes = ["servers:read", "databases:read", "postgres:read", "snapshots:read", "events:read"];
const activityApi = createScopedApi(activityScopes);
let activityOk = true;
try {
  fetchCalls.length = 0;
  await activityApi.listServers();
  await activityApi.listDatabases();
  await activityApi.listPostgresInstances();
  await activityApi.listSnapshots();
  await activityApi.recentEvents(10);
} catch {
  activityOk = false;
}
check("Activity's declared read-only scopes cover every call it actually makes", activityOk && fetchCalls.length === 5);

console.log("=== scoped-api enforcement checks ===");
let allPass = true;
for (const [desc, ok] of checks) {
  console.log(`${ok ? "PASS" : "FAIL"} — ${desc}`);
  if (!ok) allPass = false;
}
process.exit(allPass ? 0 : 1);
