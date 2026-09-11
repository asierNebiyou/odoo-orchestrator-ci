# Odoo Orchestrator — scaffolding

This is the first real code for Odoo Orchestrator, built from
`odoo-orchestrator-technical-design.md` and tracked against
`odoo-orchestrator-task-breakdown.md` (both in the project docs). It is a
working slice, not a mockup: the Rust workspace compiles and its tests pass,
the frontend type-checks and builds, and the two talk to each other over a
real HTTP+WebSocket API that has been exercised with an actual browser
(Playwright), not just curl.

## What's real vs. what's stubbed

**Real:**
- `orchestrator-core` — the domain model (servers, databases, addons
  sources, Postgres instances), a SQLite-backed store, and an in-process
  event bus. Framework-agnostic on purpose: no `tauri` dependency, so it can
  be unit-tested here and later embedded in a real desktop shell without
  change.
- `orchestrator-core::manifest` — a hand-rolled parser for the Python
  dict-literal subset `__manifest__.py`/`__openerp__.py` files actually use
  (strings incl. triple-quoted and adjacent-concatenated, lists/tuples,
  numbers, `True`/`False`/`None`, `#` comments, trailing commas), with real
  line/column error reporting. No Python runtime dependency — see the file's
  own doc comment for why that's a deliberate choice, not a shortcut.
- `orchestrator-core::modules` — walks every addons source configured for a
  server, resolves addons-path shadowing (lowest `rank` wins a technical-name
  collision, exactly like Odoo's own load order), computes a topological
  install order over the winners (Kahn's algorithm, with real cycle
  detection), and reports unresolved dependencies and any manifest that
  failed to parse rather than silently dropping them.
- `orchestrator-core::postgres` — a real embedded-Postgres process
  supervisor (task 2.1): `initdb`/`pg_ctl`/`pg_isready` wrapped behind a
  `Stopped → Starting → Running → Stopping → Crashed` state machine, with a
  background health-check loop that detects a real out-of-band crash (not
  just a command's exit code), a generation counter so a stale watcher from
  a previous `start()` can never clobber a later one's state, and a
  `with_listener` hook that fires on every transition — including a crash
  the watcher notices on its own — so a caller never has to poll. Includes a
  genuine, tested, Unix-only privilege-drop path (`run_as`) — not needed on
  a real desktop install (the app just runs as the logged-in user, same as
  Postgres never running as root there either), but this sandbox's build
  environment runs as root, and Postgres refuses that outright, so this
  turns what would otherwise be an untestable gap into a real feature: fail
  clearly instead of hitting a cryptic Postgres-side error. See "Verifying
  the Postgres supervisor" below.
- **Postgres is not a singleton.** `Core` maintains a registry of any number
  of independent `PostgresInstance`s — each with its own port, data
  directory, and lifecycle, created/started/stopped/queried independently.
  This was an explicit product decision (see
  `odoo-orchestrator-runtime-architecture.md`'s "The decision" section):
  don't limit the app to one shared Postgres, because a real user might
  genuinely need two (a different major version under test, or just
  project isolation). `Core::create_postgres_instance` /
  `list_postgres_instances` / `get_postgres_instance` /
  `start_postgres_instance` / `stop_postgres_instance` /
  `postgres_instance_state` are the methods; each real `PgState` transition
  is mapped to a `Postgres{Starting,Running,Stopping,Stopped,Crashed}`
  domain event and appended to the same event log as everything else. See
  "Verifying the multi-instance Engine screen" below for how this was
  proven, not just asserted.
- `orchestrator-api` — the local HTTP+WebSocket API over the core. Binds to
  `127.0.0.1` on an OS-assigned port, requires a per-launch bearer token on
  every route except `/health`, streams every domain event live over
  `/ws/events`, and exposes addons-source CRUD, `GET /servers/:id/modules`
  (the module scan, run on a blocking thread so a large addons-path can't
  stall the WS broadcast loop), and `/postgres-instances` (list/create/get)
  plus `/postgres-instances/:id/{start,stop}`.
- `orchestrator-shell` — a headless binary that opens the core, starts the
  API, and prints the base URL + token. Stands in for the Tauri window until
  one can actually be built (see "Why no Tauri window yet" below).
- The **Odoo & databases** (Topology) screen in the frontend — genuinely
  wired up: it fetches real state on load, submits real create-server /
  create-database requests, and updates live when *any* client changes state
  (verified by creating a server via a raw `fetch()` while the page sat idle,
  and watching it appear with no manual refresh — see "Verifying the
  live-push path" below).
- The **Modules** screen — lets you register real addons-path directories
  per server, scan them, and see live results: modules grouped by
  provenance (private/OCA/core), the SHADOWED badge on a genuinely shadowed
  copy, the collision summary, unresolved dependencies, and any manifest
  parse failures. Verified with an actual browser click-through against a
  real on-disk shadowing collision (two directories both providing a module
  called `sale_custom`) — see "Verifying module scanning" below.
- The **Engine** screen — lists, creates, starts, and stops any number of
  independent Postgres instances, each with its own label/version/port/data
  directory, each showing its own live status (including the running PID or
  a crash's exit code). Deliberately not a single-row screen with a second
  field bolted on: creating a second instance while the first is running is
  the normal case this screen is built around, not an edge case. See
  "Verifying the multi-instance Engine screen" below.
- `orchestrator-core::python_runtime` (task 2.2) — provisions an app-owned
  Python + venv via `uv` and installs a requirements list into it, applying
  the two Odoo-specific landmines the runtime-architecture doc found:
  `psycopg2` (what Odoo pins) has no Linux/macOS PyPI wheels, so it's
  transparently substituted with `psycopg2-binary`; `python-ldap` has no
  wheels anywhere, so it's dropped on non-Windows. Verified for real: a real
  `uv python install` into an app-owned directory, a real `uv venv`, and a
  real `uv pip install psycopg2==2.9.9` that actually installs and imports
  `psycopg2-binary` — executed, not just asserted. See "Verifying the Python
  runtime provisioner" below.
- `orchestrator-core::odoo::OdooSupervisor` (task 2.2) — a generic
  process supervisor for a foreground Python server, same state-machine
  shape as `PgSupervisor` (including a `with_listener` hook), but keeping
  the live child process itself (unlike `pg_ctl`, which daemonizes):
  continuous stdout **and** stderr line streaming, crash detection via
  directly observing `child.wait()` (immediate, not polled), a raw
  TCP-connect-and-HTTP-probe readiness check, and graceful `SIGTERM`-then-
  `SIGKILL` stop. Verified against a real subprocess — see "Verifying the
  Odoo process supervisor" below for exactly what that does and doesn't
  prove.
- `orchestrator-core::pg_admin` (task 2.3) — real, Odoo-agnostic Postgres
  admin operations built directly on 2.1's running instances:
  `database_exists`, `create_database`, `drop_database`
  (`dropdb --if-exists`), `duplicate_database` (force-disconnects other
  backends on the source via `pg_terminate_backend`, then
  `CREATE DATABASE ... TEMPLATE ...` — Postgres refuses to template a
  database with live connections, so this is a real constraint the design
  had to handle, not a nicety), `dump_database` (plain-SQL `pg_dump
  --file`), and `restore_database` (fresh `createdb`, replay via `psql`).
  `Core::create_database` is now genuinely async and Postgres-backed
  (previously just a SQLite insert), and `duplicate_database` /
  `drop_database` / `backup_database` / `restore_database` are new real
  `Core` methods — each validates its target server's `PostgresInstance`
  is actually running first, and each emits the matching
  `Database{Created,Duplicated,Dropped,BackedUp,Restored}` event. Exposed
  via `orchestrator-api` as `POST /databases/:id/duplicate`,
  `DELETE /databases/:id`, `POST /databases/:id/backup`, and
  `POST /servers/:id/databases/restore`. **This also resolved the
  previously-open "which `PostgresInstance` does a server use" question**:
  `OdooServer` now carries a required `postgres_instance_id`, validated at
  `create_server` time — the caller decides, per the topology rule (share
  by default, separate only when something genuinely forces it), rather
  than `Core` silently assuming "the only instance." The **Odoo & databases**
  (Topology) screen has a Postgres-instance picker for server creation and
  real Duplicate/Backup/Restore/Drop buttons per database. See "Verifying
  the database lifecycle commands" below for how all of this was checked
  against real Postgres, real data, the real HTTP API, and a real browser —
  not mocks.
- `orchestrator-core::filestore` + `Snapshot` (task 3.1) — a named,
  point-in-time-frozen snapshot combining a real Postgres database duplicate
  (reusing 2.3's `duplicate_database` mechanism exactly) and an optional
  hardlink-based filestore directory copy. `filestore.rs` is fully generic —
  hardlinks every regular file (`std::fs::hard_link`, instant and
  metadata-only), recreates the directory structure, and recreates symlinks
  as symlinks rather than following them. `Core::create_snapshot` derives a
  collision-free snapshot database name, duplicates the source, optionally
  snapshots a filestore directory, and emits `SnapshotCreated`;
  `Core::delete_snapshot` tears both halves down and emits `SnapshotDeleted`.
  A `Snapshot` stores `server_id` directly (not just `database_id`), so it
  stays fully usable after its source `Database` is dropped — proven, not
  just asserted, by a dedicated test that drops the source mid-test and
  confirms the snapshot can still be cleanly deleted afterward. Exposed via
  `orchestrator-api` as `/databases/:id/snapshots` (list/create), `/snapshots`
  (list all), and `/snapshots/:id` (delete); the **Odoo & databases**
  (Topology) screen has a real Snapshot action per database plus an indented
  list of existing snapshots with their own Delete actions. See "Verifying
  the snapshot primitive" below.
- `Core::revert_database_to_snapshot` (task 3.3) — "one-click revert": drops
  a database's real Postgres contents and re-creates them **in place** from
  an existing snapshot (`duplicate_database` reused a second time, direction
  reversed from `create_snapshot`), optionally replacing a filestore
  directory the same way. Genuinely different from `create_snapshot`/
  `delete_snapshot` (never touch a live database) and from `restore_database`
  (task 2.3 — always creates a brand-new database, never overwrites one).
  Validates the snapshot actually belongs to the database being reverted and
  that a filestore target is only accepted when the snapshot has a filestore
  half. When asked, takes a real counter-snapshot of the live database
  *before* the destructive drop — the friction ladder's "counter-snapshot
  offered" element for this rung (`odoo-orchestrator-ui-design-principles.md`
  rates "restore snapshot over live DB" rung 3, its second-highest), proven
  for real rather than promised in a dialog: the counter-snapshot is
  returned to the caller and is itself a normal, listable, deletable
  `Snapshot`. Exposed via `orchestrator-api` as `POST /databases/:id/revert`;
  the **Odoo & databases** (Topology) screen has a real "Revert to this"
  action on every snapshot row, with a confirmation dialog carrying all four
  elements the design doc's friction ladder calls for (named object,
  quantified consequence, reversibility status, ripple effects). See
  "Verifying one-click revert" below.
- `orchestrator-core::git` + `Core::current_git_branch` (task 3.2) — reads
  the real current branch of a real git working directory via the real
  `git` CLI (`git -C <path> rev-parse --abbrev-ref HEAD`), fully generic and
  Odoo-agnostic like `pg_admin.rs`/`filestore.rs`. Reuses the existing
  `Private`-kind `AddonsSource` as "the linked repo" — an earlier session's
  own test already called one `"this repo"` — rather than inventing a new
  per-server field. Distinguishes every real failure mode instead of
  collapsing them: not a repo at all, no commits yet (a real, observed `git`
  quirk — the command fails but still prints `"HEAD"` to stdout, which is
  why this checks the exit code rather than trusting stdout), and detached
  HEAD (a *successful* command that still has no branch name). `Core`
  collapses all of those, plus "no private source registered," to a plain
  `Ok(None)` — a snapshot's name has always been free-form text, so a
  missing suggestion is a normal case, not an error. Exposed via
  `orchestrator-api` as `GET /servers/:id/git-branch`; the Topology screen's
  Snapshot prompt now prefills its default value with the real branch name
  when one is found. See "Verifying git-branch binding" below. This task
  turned out not to need what an earlier version of this README implied —
  a real *published* (GitHub-hosted) repo — only the local `git` binary and
  a locally-creatable one; that's a meaningfully different, much easier bar
  than 2.2's genuinely-still-blocked GitHub dependency below.
- **A real, enforced plugin permission/loader architecture (task X.1)** —
  `frontend/src/lib/scopes.ts` defines 13 `resource:read`/`resource:write`
  permission scopes covering the entire api surface;
  `frontend/src/lib/scopedApi.ts`'s `createScopedApi(scopes)` wraps the real
  `api` client so every method checks the caller's granted scopes *before*
  making the real HTTP call, throwing `ScopeDeniedError` otherwise —
  enforced, not just declared. `frontend/src/lib/plugins.ts`'s `PLUGINS`
  registry replaces `App.tsx`'s old hardcoded `Screen` union: the nav rail
  and screen routing are now derived from it, and each plugin (Topology,
  Engine, Modules, Activity) only receives the scopes it actually needs —
  every existing screen was refactored to receive `api`/`subscribeEvents`
  via props instead of importing the singleton directly, so there's no back
  door around its declared scopes. Proven with a Node-level unit script
  (`scopedapi_test.mjs`, run via `vite-node` against the real TypeScript
  source with a stubbed `fetch`) confirming a denied call throws before any
  network request is made, and a fresh Playwright pass
  (`e2e_plugins_activity.mjs`) confirming the refactored screens still work
  end to end against the real running API. See "Verifying the plugin
  architecture and Activity view" below.
- **An Activity screen (task X.2)** — `frontend/src/screens/Activity.tsx`, a
  new plugin registered purely by an entry in `plugins.ts` (the concrete
  proof the X.1 loader works for a screen it didn't ship with), surfacing
  the already-persisted event log per
  `odoo-orchestrator-ui-design-principles.md`'s "instrument, not scoreboard"
  reframe: real "protection coverage" (N of M databases with ≥1 real
  snapshot), a real 12-week activity heatmap bucketed by actual event
  timestamps with **no streak counter**, and a human-readable description
  for every one of the 21 real event kinds (resolving ids to real
  server/database/instance/snapshot names, not raw JSON). Time-saved/
  currency facts are explicitly marked "not shown yet" rather than
  fabricated, since they need task 2.4's not-yet-built module hash-compare
  data. Activity's declared permissions are entirely read-only. See
  "Verifying the plugin architecture and Activity view" below.
- **Real Odoo server supervision and module install/upgrade/uninstall
  (tasks 2.2/2.4)** — `Core::start_server`/`stop_server` render a real
  `odoo.conf` from a server's addons sources and running `PostgresInstance`
  and spawn/supervise real `odoo-bin`, with server state persisted through
  every transition; `Core::install_modules`/`upgrade_modules`/
  `uninstall_modules` run real one-shot `odoo-bin -i`/`-u`/`shell` commands
  per database, with a real post-upgrade module version read back from
  `ir_module_module` rather than fabricated. Verified end-to-end against a
  real Odoo 17.0 checkout — see "Verifying real Odoo server supervision and
  module commands" below for the full detail, including three real
  Odoo/Postgres compatibility issues found and fixed along the way.
- `orchestrator-core` has **99 passing tests** (`cargo test --workspace`):
  domain/event-bus tests, the manifest parser and shadowing/graph logic
  (exercised against real Odoo-shaped manifest fixtures written to a
  tempdir), the Postgres supervisor (exercised against a real installed
  Postgres 16, including a real `kill -9` sent to the live postmaster to
  prove crash detection actually works), `Core`-level tests including ones
  that create two distinct Postgres instances and start/stop them
  independently and a full set covering the real database lifecycle
  (scoping, rejection when the instance isn't running, duplicate/drop/
  backup/restore all exercised against a real Postgres instance `Core`
  starts itself), the Python runtime provisioner (real `uv`/PyPI), the Odoo
  process supervisor (a real subprocess standing in for `odoo-bin` — see
  below for why), the Postgres admin operations themselves (a full
  dump→drop→restore round trip and a duplicate test with real inserted
  data, both against real running clusters), the filestore hardlink
  primitive (real inode-identity checks, real symlink recreation, the
  documented shared-inode edit leak proven deliberately), and the snapshot
  primitive itself (real data frozen at snapshot time, real hardlinked
  files, real deletion of both halves, and the source-database-dropped
  survival test), and one-click revert (real live data actually overwritten
  in place, a real counter-snapshot proven to capture the pre-revert state
  rather than the target state, both validation errors, and a real filestore
  target actually replaced rather than merged), and git-branch binding
  (a real branch switch, a non-"main" default branch name, the realistic
  subdirectory-of-a-repo case, a real detached HEAD, and a real zero-commit
  repo) — none of it hand-built data structures standing in for the real
  thing.

**Stubbed:**
- The Modules screen still doesn't show an "installed" or "up to date"
  column — `Core::install_modules`/`upgrade_modules`/`uninstall_modules`
  (task 2.4) are real and tested, so the real `ir_module_module` state they
  need is genuinely available now, but the frontend buttons/instrument
  themselves (the rest of task 2.5) haven't been built yet. This is now a
  "not yet built" gap, not a "can't build it here" one.
- The frontend has no server Start/Stop controls yet either — same
  situation: `Core::start_server`/`stop_server` (task 2.2) and their
  `POST /servers/:id/{start,stop}` routes are real and tested, the buttons
  just don't exist on the Topology screen yet (also task 2.5).
- `backup_database` currently writes a plain-SQL `pg_dump` (the "dump"
  format, filestore not included) rather than Odoo's own "zip" format
  (`dump.sql` + `manifest.json` + filestore) — the right scope today since
  there's no real Odoo filestore to back up yet, but worth extending
  alongside real Odoo wiring, since `odoo-orchestrator-runtime-architecture.md`
  flags that a filestore-less restore fails *silently* rather than loudly.
- `PgSupervisor`'s Tauri-sidecar packaging (spike 0.7) is still open — it
  needs an actual desktop environment this sandbox doesn't have. The
  supervision and multi-instance logic itself no longer needs a desktop to
  validate (see below); only the sidecar mechanism does.

## Why no Tauri window yet

This sandbox has `cargo`, `rustc`, and `node`, but is missing the webview
dev libraries Tauri needs to link against on Linux (`webkit2gtk-4.1` /
`webkit2gtk-4.0`). Rather than fake a window or leave a build that silently
fails, the `tauri` dependency is commented out in
`src-tauri/Cargo.toml` and `orchestrator-shell` runs `orchestrator-core` +
`orchestrator-api` headless. This is a deliberate, temporary state, not a
design decision — the two crates that matter (`orchestrator-core`,
`orchestrator-api`) have zero UI-framework dependency, so nothing about them
needs to change when a real desktop environment is available. At that point:

1. Uncomment the `tauri` dependency in `src-tauri/Cargo.toml`.
2. Turn `main.rs` into a real Tauri `Builder::default()` app that starts the
   core+API in setup and opens a window pointed at the frontend
   (`tauri.conf.json` already has the dev URL and CSP scoped to
   `127.0.0.1:*` for this).
3. Pass the API's base URL and bearer token into the frontend via a Tauri
   `invoke` call or an injected global at boot, instead of the
   `VITE_API_BASE_URL` / `VITE_API_TOKEN` build-time env vars used for now
   (flagged in `frontend/src/lib/api.ts`).

Validation spike 0.6 (openvscode-server sidecar in a Tauri webview) is
blocked on the same thing. Spike 0.7 (Tauri sidecar lifecycle for a
long-running Postgres process) is narrower than it used to be: the actual
process-supervision *logic* it was meant to validate — start, health-check,
graceful stop, crash recovery, and now the multi-instance registry sitting
on top of all of that — is now built and tested standalone in
`orchestrator-core::postgres`/`Core` (see below), so what's left to validate
on a real desktop is specifically whether Tauri's sidecar API can host that
existing logic, not whether the logic itself works.

## Running it

**Backend:**
```
cd src-tauri
cargo run --bin orchestrator-shell
```
This prints the bound address and a bearer token, e.g.:
```
orchestrator-api listening on http://127.0.0.1:41213
bearer token: x5c06bKv53XWq37YUMkbTBeWLRU5eqTt
```
State persists to `.orchestrator-data/orchestrator.sqlite3` (relative to
wherever you run it) unless `ORCHESTRATOR_DATA_DIR` is set.

**Frontend:**
```
cd frontend
npm install
cat > .env.local <<EOF
VITE_API_BASE_URL=http://127.0.0.1:<port from above>
VITE_API_TOKEN=<token from above>
EOF
npm run dev
```
Then open the printed `localhost:5173` URL. `.env.local` is git-ignored dev
wiring, not how the real app will get its token (see step 3 above).

**Checks:**
```
cd src-tauri && cargo build --workspace && cargo test --workspace
cd frontend && npm run typecheck && npm run build
```

## Verifying the live-push path

The event bus is the point of this architecture — every surface (shell UI,
future editor extension, future plugins) is supposed to react to state
changes instead of polling. That only means something if a change made by
*one* client shows up in *another* client without a manual refresh. This was
checked with a Playwright script, not just by reading the code:

1. Start the backend and frontend as above; open the frontend in a
   Playwright-controlled browser; confirm it shows whatever servers already
   existed at page load (proves the initial fetch path).
2. With the page left completely untouched, issue a `POST /servers` request
   directly against the API with plain `fetch()` — not through the page.
3. Wait 800ms with no interaction, then read the page's rendered text again.

The new server appeared, and the event log showed it correctly ordered by
timestamp — confirming the WebSocket push actually updated the UI. This
caught a real bug along the way: the first pass at this test showed the
create-server *button* working, but that only proved the button's own
explicit refresh-after-submit call worked, not the push path. A second,
more careful test (out-of-band creation, page left alone) showed
`/ws/events` was silently failing for every browser client, because
**browsers cannot set an `Authorization` header on a native WebSocket
handshake**. The fix: `/ws/events` is exempted from the header-based auth
middleware (`auth.rs`) and instead validates a `?token=` query parameter
inside the WebSocket handler itself, before upgrading the connection
(`ws.rs`); the frontend passes the token the same way (`api.ts`). This is
worth knowing before "fixing" WebSocket auth issues elsewhere in this
codebase the same way header auth is done for REST routes — it doesn't work
for WebSocket.

One console warning shows up during that test — `WebSocket connection ...
failed: WebSocket is closed before the connection is established` — and is
expected, not a regression: React 18's `<React.StrictMode>` (on in
`main.tsx`) double-invokes effects in dev, so the first `subscribeEvents()`
call's socket gets torn down by its own cleanup before the handshake
finishes, and the second call's socket is the one that actually receives
events. It doesn't appear in a production build. (This same warning
reappears harmlessly in the Engine screen's own E2E test below — it's the
same dev-mode double-invoke, not a new issue.)

## Verifying module scanning

The shadowing detector's whole reason to exist is a bug class Odoo itself
can't see, so "it compiles and the unit tests pass" isn't convincing on its
own — it was checked against a real filesystem layout with a genuine
collision, through the actual UI, not just the API:

1. Two real addons directories were created on disk: a `private` one with
   `base`, `sale`, and a custom `sale_custom` module, and an `oca` one that
   happens to also ship a module technically named `sale_custom` (a
   plausible real-world accident — a client's custom module and an OCA
   module landing on the same name) plus an unrelated `sale_workflow`.
2. Both were registered as addons sources on a server (`private` at rank 0,
   `oca` at rank 1) through the real `POST /servers/:id/addons-sources`
   endpoint.
3. A Playwright browser was driven to the Modules screen, clicked "Scan
   modules", and the rendered page was checked for: the private
   `sale_custom` present, the OCA `sale_custom` marked `SHADOWED`, the
   collision count line reading "1 shadowed by addons-path order", both
   provenance groups rendered, and — importantly — that the page makes no
   claim about install/upgrade state, since that isn't real yet.

All of those checks passed against the rendered page text, and the API
response (checked separately with curl) also had the correct topological
install order (`base, sale, sale_custom, sale_workflow`) and zero unresolved
dependencies. The Rust side has the same scenario as a fast unit test
(`modules::tests::lower_rank_source_wins_a_shadowing_collision`), plus
dedicated tests for a genuine dependency cycle, a missing addons directory,
and a manifest that fails to parse — each proven to be reported without
aborting the rest of the scan.

## Verifying the Postgres supervisor

Task 2.1's whole point is a state machine that's honest about what's really
happening to a real process, so its test suite deliberately doesn't stop at
"the commands ran and returned 0":

1. **Full lifecycle** — start a real cluster in a tempdir, confirm `pg_isready`
   actually reports ready (not just that `pg_ctl` exited 0), stop it, confirm
   it's actually unreachable afterward.
2. **Double-start is rejected**, not silently turned into a second instance
   competing for the same port.
3. **`ensure_initdb` is idempotent** — the second call recognizes the
   existing cluster via `PG_VERSION` and does nothing, rather than re-running
   (and likely failing) `initdb` against a non-empty directory.
4. **Real crash detection**: start the supervisor, then send an actual
   `kill -9` to the live postmaster PID completely out-of-band (not through
   `stop()`) — exactly what an OOM killer or a user's own `kill` would do —
   and confirm the background health watcher notices within a few polling
   cycles and transitions to `Crashed`, without ever being told to stop.
5. **The generation guard**: stop then immediately restart, and confirm the
   *first* run's health watcher (still finishing its polling loop in the
   background) can't overwrite the second run's `Running` state — a race
   that would otherwise be easy to introduce and hard to notice, since it'd
   only show up as an intermittent, timing-dependent test flake.
6. **The state listener fires on every transition**, including a crash the
   health watcher detects on its own with no external caller ever telling it
   to change state — this is the mechanism `Core` relies on to turn a
   `PgState` change into a domain event without polling.

All 7 tests run against an actually-installed Postgres 16 (via
`discover_system_bin_dir()`, a dev/test-only convenience — the shipped app
always gets `bin_dir` from its own bundled runtime instead) and passed
consistently across repeated runs, both serial and parallel. Because this
sandbox's build environment runs as root and Postgres refuses that outright,
the tests exercise the `run_as` privilege-drop path for real via
`sandbox_root_workaround_user()` (itself a no-op on any real desktop
install): drop to this sandbox's unprivileged `claude` account, having
`chown`ed the tempdir to it first, before spawning
`initdb`/`postgres`/`pg_ctl` — the same code path a real desktop install
simply never triggers, proven to actually work rather than left as untested
(would-be-dead) code. `Core::get_or_create_supervisor` applies the same
workaround automatically, which is what lets the multi-instance tests below
start real Postgres processes too.

## Verifying the multi-instance Engine screen

The point of this slice was a specific product decision — "we might need
multiple Postgres instances, let's not limit it to one" — so the test that
actually matters is proving two *independent* instances behave
independently, not just that the code compiles. This was checked at three
layers:

1. **`Core`-level (`two_distinct_postgres_instances_run_independently`)**:
   create two `PostgresInstance`s with different ports and data
   directories, start the first, confirm the second is still `Stopped`,
   start the second, confirm both are now `Running` with distinct PIDs, stop
   the first, confirm the second is still `Running`, then stop the second.
   Runs against two real Postgres clusters, not mocks.
2. **API-level**: `GET /postgres-instances` returns both instances with
   their live `state` folded in (config from SQLite, state from the
   in-memory supervisor registry), and `POST /postgres-instances/:id/start`
   and `/stop` genuinely start/stop the right one, checked with curl against
   a running `orchestrator-shell`.
3. **Browser-level (Playwright, `e2e_engine.mjs`)**: drove the real Engine
   screen to create two instances ("pg-a", Postgres 16; "pg-b", Postgres
   15), started pg-a and confirmed pg-b stayed `stopped`, started pg-b and
   confirmed pg-a stayed `running`, stopped pg-a and confirmed pg-b stayed
   `running`, then stopped pg-b. All 8 assertions passed, including both
   instances actually reaching `running` (with real PIDs visible in the
   rendered page) and both cleanly reaching `stopped` again — this is a
   real browser clicking real buttons against a real API starting and
   stopping two real `postgres` processes, not a UI mockup of what
   multi-instance would look like.

## Verifying the Python runtime provisioner

`python_runtime.rs`'s whole point is that asking for Odoo's actual pinned
dependencies (`psycopg2`, not `psycopg2-binary`) would otherwise fail on
Linux/macOS, so the test that matters is proving the substitution produces a
real, working install, not just that the string-rewriting logic looks right:

1. **Pure logic** (no network): `psycopg2` and `psycopg2==2.9.9` both
   rewrite to `psycopg2-binary`; `python-ldap==3.4.3` is dropped entirely;
   unrelated requirements (`Babel==2.9.1`, `lxml>=4.6.0`) pass through
   untouched; a realistic four-package Odoo requirements slice is handled
   correctly all at once.
2. **Real provisioning**: `uv python install 3.11` into a fresh temp
   directory (via `UV_PYTHON_INSTALL_DIR`, not wherever `uv` would install
   by default — proving the "app-owned, not system-wide" claim), a real
   `uv venv` from that Python, and a real `uv pip install psycopg2==2.9.9`
   against that venv. The test then actually runs the provisioned
   interpreter with `import psycopg2; print(psycopg2.__version__)` and
   checks it succeeds and prints something — proving the *substituted*
   package is what got installed and that it actually imports, not just
   that some `pip install` command exited 0.
3. **Idempotency**: calling `ensure_venv` a second time for the same
   directory returns the same python path without recreating the venv.

All 7 tests passed, including the two that hit real `uv` and real PyPI —
consistent with this project's standing rule of testing against the real
tool, not a stand-in, wherever the real tool is actually reachable from this
sandbox (unlike Odoo's own source — see below).

## Verifying the Odoo process supervisor

Task 2.2's process-supervision half needs the same honesty `postgres.rs` and
`python_runtime.rs` insist on — but here that honesty cuts against fully
proving the "runs Odoo" claim, and it's worth being explicit about exactly
where the line falls:

1. **Full lifecycle** — start a real Python HTTP server standing in for
   `odoo-bin` (see below for why it's a stand-in, not the real thing),
   confirm the readiness probe genuinely detects it serving HTTP (not just
   that a process exists), stop it gracefully, confirm the port is
   unreachable afterward.
2. **Double-start rejected**, and stopping an already-stopped supervisor is
   a harmless no-op — same shape as the equivalent Postgres tests.
3. **Log streaming from both streams**: the stand-in prints one line to
   stdout and one to stderr on startup, plus a third stdout line once it's
   actually bound its port; all three arrive at the listener, tagged with
   the correct stream, in the order they were printed.
4. **Real crash detection**: `kill -9` the live process's pid completely
   out-of-band, confirm the supervisor notices via `child.wait()` returning
   and transitions to `Crashed` — no polling interval to wait out, unlike
   Postgres's `pg_isready`-based detection, because this supervisor holds
   the actual child process instead of a daemonized pidfile.
5. **Startup timeout**: a stand-in that deliberately never binds its port
   causes `start()` to time out and return an error, and the test confirms
   the process actually gets killed (not left running and merely
   forgotten about).

All 6 tests passed against a real subprocess. At the point this section was
first written, what this didn't yet prove was that this supervisor could
actually run `odoo-bin` — the environment it was built in had no legitimate
way to reach real Odoo source (only distributed from GitHub, which that
sandbox's egress policy blocked). `OdooSupervisor` was deliberately built
generic — an arbitrary interpreter path, working directory, argv, and
environment, nothing Odoo-specific except which port it probes for
readiness — precisely so running real `odoo-bin` would need zero code
changes once a real checkout was available to test against. That's now
happened; see the next section for the real verification.

## Verifying real Odoo server supervision and module commands

Tasks 2.2's `Core` wiring and 2.4 were finished on a real local desktop
against a real Odoo 17.0 checkout — the actual target the previous section
said this project was deliberately, honestly stopping short of. Three
layers, all against real `odoo-bin`, not a stand-in:

1. **Manual verification first, outside the test harness, before trusting
   an automated test with it**: a real Postgres cluster started by hand, a
   hand-written `odoo.conf`, real `odoo-bin` run directly under a real
   provisioned venv — `curl`'d and got back a genuine `303 → /web` redirect
   with a real session cookie. This is how the first two of the three real
   compatibility issues below were actually found (they surfaced as plain
   Python tracebacks in `odoo-bin`'s own stderr, not as a Rust test
   failure) before the automated test was written to encode them.
2. **`Core::start_server`/`stop_server`** (`start_server_runs_a_real_odoo_bin_and_serves_http`):
   registers a real checkout's addons directory as a server's addons
   source, starts real `odoo-bin` through `Core`, confirms it's actually
   serving HTTP and that `OdooServer::state` reads back `Running` through
   `Core::get_server` (not just held in an in-memory supervisor, unlike
   `PostgresInstance`'s own state), stops it, confirms `Stopped`.
3. **`Core::install_modules`/`upgrade_modules`/`uninstall_modules`**
   (`install_upgrade_and_uninstall_a_real_module_against_a_real_database`):
   installs a real module (`google_account`) into a bare database — proving
   `-i` on a never-initialized database genuinely bootstraps the whole
   framework, since every module depends on `base` — and confirms
   `ir_module_module.state` actually reads `installed` afterward, not just
   that the command claimed success; upgrades it and confirms a real,
   non-placeholder version came back on the emitted event; uninstalls it
   through `odoo-bin shell` and confirms `ir_module_module.state` actually
   reads `uninstalled`.

**Three real Odoo/Postgres compatibility issues found and fixed along the
way** — the kind of thing no amount of testing against a stand-in process
could have surfaced, and exactly why this project treats "verified against
the real thing" as different from "verified against a faithful-looking
substitute":

- Real Odoo hard-refuses to start (`"Using the database user 'postgres' is
  a security risk, aborting."`) when its own `db_user` is the cluster's
  superuser role — which every other Postgres-touching `Core` method
  deliberately *does* connect as, since this project's own cluster has
  exactly one admin role. Fixed with a new `pg_admin::ensure_role`, giving
  Odoo its own dedicated non-superuser `odoo` role.
- Recent `setuptools` releases (81+) dropped `pkg_resources`, which Odoo's
  own code (`odoo/modules/module.py`) imports directly, independent of
  anything pip itself needs. `requirements.txt` doesn't pin `setuptools`
  itself, so a freshly provisioned venv resolves the latest one and real
  `odoo-bin` fails at import time. Fixed by pinning `setuptools<81`.
- Postgres 15+ revoked the default `CREATE`-on-`public`-schema privilege
  every earlier version granted non-owner roles, so the new `odoo` role
  above could authenticate fine but then fail with `permission denied for
  schema public` the instant Odoo tried to create its first table. Fixed
  with a new `pg_admin::grant_schema_privileges`, called per-database
  (schema privileges are per-database, so this can't be folded into
  `ensure_role`'s one-time setup).
- A fourth, frontend-side one, found while building 2.5's currency
  instrument: `ir_module_module`'s Python model names its *stored* column
  (the version actually installed) `latest_version` — labeled "Installed
  Version" in its own field definition — while its `installed_version`
  attribute is a `compute=`d field (labeled "Latest Version") that re-walks
  the addons path at read time and isn't a real column `SELECT` can reach
  at all. `Core::database_module_states` sources its own `installed_version`
  field from Odoo's stored `latest_version` column, renamed to what it
  actually represents rather than propagating Odoo's own inverted naming;
  and comparing it against what's genuinely on disk needs Odoo's own real
  version-adaptation rule (`odoo/modules/module.py`'s `adapt_version`: a
  missing manifest version defaults to `"1.0"`, then gets prefixed with the
  server's own major series unless already prefixed), replicated in
  `Modules.tsx` rather than guessed — a naive direct comparison would have
  reported nearly every real module as perpetually out of date.

**Task 2.5's frontend wiring on top of this, verified end-to-end against a
real running server, real `odoo-bin`, and a real browser** (a new Playwright
script, `e2e_server_and_module_commands.mjs`): `Topology.tsx`'s new
Start/Stop buttons take a server from `stopped` through a real `odoo-bin`
boot to `running` — confirmed with a real `curl` to the now-running port
returning `200`, not just the UI's own claimed status — then back to
`stopped`. `Modules.tsx`'s new Install button takes a real module
(`google_account`) from "not installed" to a real `UP TO DATE` badge, and
the currency instrument correctly reads "11 of 559" afterward — not "1 of
559": installing pulls in real framework dependencies (`base_setup`,
`bus`, `iap`, `auth_totp`, …), and every one of them is correctly reported
up to date, proof the version-adaptation comparison works across many real
modules with real version numbers, not one coincidentally-simple case.
Uninstall reverts the badge to "not installed". The existing regression
suite (`e2e_engine.mjs`, `e2e_database_lifecycle.mjs`,
`e2e_plugins_activity.mjs`, `e2e_snapshot.mjs`, `e2e_revert.mjs`) was
re-run against this change on a real browser and passes unchanged;
`e2e_git_branch.mjs` needs out-of-band fixture state (a specific git
repo/branch) this pass didn't reconstruct, unrelated to anything touched
here.

## Verifying the database lifecycle commands

Task 2.3's operations do real, sometimes destructive things to a real
Postgres cluster, so "the SQL ran" isn't enough — the point was proving real
data actually survives (or is actually gone) at every step, checked at three
independent layers:

1. **`pg_admin.rs` unit tests (3, against real running Postgres clusters)**:
   `create_exists_and_drop_a_real_database` covers the basic CRUD surface;
   `duplicate_database_carries_over_real_data` inserts a real row into a
   source database, duplicates it, confirms the row is present in the copy,
   *and* confirms the source itself still works normally afterward (proving
   the force-disconnect-then-template dance doesn't leave the source
   half-broken); `dump_and_restore_round_trips_real_data` does a full
   dump → drop → restore cycle and confirms the restored database has the
   original data, not just that each command exited 0.
2. **`Core`-level tests (6, against a real Postgres instance `Core` starts
   itself)**: scoping databases to their server, rejecting `create_database`
   with `PostgresInstanceNotRunning` when the instance is genuinely not
   running (not just when the row doesn't exist), and duplicate/drop/
   backup+restore all exercised through `Core`'s actual async methods —
   including asserting the drop removes the SQLite row too, and that backup
   sets `last_backup_at` on the surviving row.
3. **End-to-end against the real running API and a real browser**: a `curl`
   pass against a real `orchestrator-shell` process — create a Postgres
   instance, start it, create a server against it, create a database, insert
   a real row via `psql`, duplicate the database *over the API*, confirm the
   row is in the copy via `psql`, back it up over the API, drop the original
   over the API, confirm via `psql -l` it's actually gone from Postgres (not
   just the SQLite row), restore the backup over the API into a new
   database, confirm the row survived — plus a from-scratch Playwright
   script (`e2e_database_lifecycle.mjs`) driving the exact same lifecycle
   through the real Topology screen's Duplicate/Backup/Restore/Drop buttons
   (including intercepting the `window.prompt`/`confirm` dialogs those
   actions use), against the real running shell binary. All checks passed
   at every layer; every domain event
   (`database_created`/`duplicated`/`backed_up`/`dropped`/`restored`)
   appeared in `/events` in the right order.

## Verifying the snapshot primitive

Task 3.1's headline claim is that a snapshot is frozen at the moment it's
taken, not a live view of the source — so the tests that matter are the ones
that actually mutate the source *after* snapshotting and confirm the
snapshot didn't move, checked at four independent layers:

1. **`filestore.rs` unit tests (8, against a real filesystem)**: every
   regular file is hardlinked and directory structure recreated; hardlinked
   files are confirmed to genuinely share the same inode
   (`std::os::unix::fs::MetadataExt::ino()`), not just contain equal bytes;
   deleting and recreating a file in the source afterward does *not* affect
   an existing snapshot (the safe pattern for Odoo's real, content-hash-
   addressed filestore); editing a file in place after snapshotting *does*
   leak through the shared inode (the documented, deliberately-proven
   limitation of the generic primitive); symlinks are recreated as symlinks,
   not followed; empty directories snapshot cleanly; a missing source or an
   already-existing destination is rejected rather than silently
   overwritten.
2. **`Core`-level tests (7, against a real Postgres instance and real
   files)**: `create_snapshot_duplicates_real_data_database_only` inserts a
   real row, takes a snapshot, mutates the *source* afterward, and confirms
   the snapshot's own database still has the old value — the actual
   frozen-at-the-moment-it-was-taken proof, not a comment claiming it;
   `create_snapshot_with_filestore_hardlinks_real_files` confirms a real file
   in a real filestore directory is hardlinked with correct bytes;
   `delete_snapshot_removes_its_database_and_filestore_copy` confirms both
   the Postgres database and the filesystem directory are actually gone
   afterward, and that `get_snapshot` then returns `NotFound`;
   `snapshot_survives_and_stays_deletable_after_its_source_database_is_dropped`
   drops the source database mid-test and confirms the snapshot is still
   listed, its own Postgres database still exists, and it can still be
   cleanly deleted through `Core`; `list_snapshots_scopes_to_database_when_asked`
   confirms filtering across multiple databases' snapshots;
   `create_snapshot_emits_event_with_filestore_flag` confirms `SnapshotCreated`
   carries the correct `has_filestore` value and `SnapshotDeleted` fires on
   delete.
3. **End-to-end against the real running API**: a `curl`+`psql` pass — insert
   a real row, take a snapshot over the API, mutate the *live* database
   afterward, confirm via `psql` that the snapshot's own database still has
   the old value. This is the same freeze claim as the `Core`-level test
   above, proven again one layer up through the real HTTP surface.
4. **Browser-level (Playwright, `e2e_snapshot.mjs`)**: drove the real
   Topology screen's Snapshot and Delete buttons end to end against the real
   running shell binary (window prompt/confirm dialogs intercepted), creating
   a Postgres instance, server, and database, taking a snapshot named
   "pre-migration", confirming it renders in the UI, deleting it, and
   confirming it's gone while the source database is untouched. All 4
   assertions passed. This script deliberately doesn't re-prove the freeze
   claim itself (already proven twice at lower layers above) — its job is
   specifically confirming the real UI buttons drive the same server-side
   mechanism correctly.

## Verifying one-click revert

Task 3.3's headline claim is the opposite of 3.1's: reverting must actually
*overwrite* a live database's real data with a snapshot's frozen data, in
place — so the tests that matter mutate the live database, revert, and
confirm the live data actually moved, checked at four independent layers:

1. **`Core`-level tests (5, against a real Postgres instance and real
   files)**: `revert_database_to_snapshot_restores_real_data_in_place`
   inserts a row, snapshots it, mutates the *live* database afterward, reverts,
   and confirms the live database's real data is back to the snapshot's frozen
   value — the actual overwrite proof, not a comment claiming it — while also
   confirming the `Database` row itself (id, name) is unchanged throughout;
   `revert_database_to_snapshot_takes_a_real_counter_snapshot_when_asked`
   confirms the counter-snapshot taken during revert holds the *pre-revert*
   live value, not the target snapshot's value (otherwise it would be useless
   as an undo for the revert itself), and that it shows up as a real, listable
   snapshot and in the event log; `revert_database_to_snapshot_rejects_a_snapshot_from_a_different_database`
   and `revert_database_to_snapshot_rejects_filestore_target_when_snapshot_has_none`
   cover both validation errors; `revert_database_to_snapshot_with_filestore_hardlinks_the_snapshots_files_into_target`
   populates a "live" filestore directory with different content first and
   confirms revert actually replaces it (not merges into it) with the
   snapshot's hardlinked files.
2. **End-to-end against the real running API**: a `curl`+`psql` pass — insert
   real data, snapshot it, mutate the live database, revert over the API with
   a counter-snapshot name, confirm via `psql` the live database is back to
   the snapshot's value *and* the counter-snapshot holds the pre-revert
   value, then confirm a revert using a snapshot that belongs to a different
   database is rejected with `422`.
3. **Browser-level (Playwright, `e2e_revert.mjs`)**: drove the real Topology
   screen's new "Revert to this" button end to end against the real running
   shell binary — creating a Postgres instance, server, database, and a
   snapshot named "v1", then clicking "Revert to this" and capturing the
   actual `window.confirm()` dialog text rather than just clicking through
   it. All 8 assertions passed, including four that check the confirmation
   dialog genuinely contains each of the friction ladder's four required
   elements (named object, quantified consequence, reversibility status,
   ripple effects) — not just that a dialog appeared — and a final one
   confirming a real counter-snapshot (`pre-revert-<ISO timestamp>`)
   actually appeared in the UI afterward.
4. **What this doesn't (and can't yet) claim**: native `window.confirm()`
   can't relabel its own button to name the action the way the design doc's
   ideal calls for, so the button itself still says "OK" rather than
   "Revert to v1" — the four copy elements live in the message body instead.
   That's a real, called-out gap against the design doc's ideal, not
   something the tests paper over.

## Verifying git-branch binding

Task 3.2's headline claim is that the Snapshot prompt's suggested name is
the repo's *actual* current branch — not a hardcoded guess, not always
"main" — so the tests that matter switch branches, check subdirectories,
and induce the real edge cases (no commits, detached HEAD), checked at four
independent layers:

1. **`git.rs` unit tests (7, against real local git repositories)**:
   `reports_the_real_current_branch_name` and
   `does_not_hardcode_an_assumed_default_branch_name` (a repo initialized
   with `-b trunk` reports "trunk", not "main"); `tracks_a_real_branch_switch`
   confirms a real `git checkout -b` is picked up; `finds_the_enclosing_repo_from_a_subdirectory_not_just_the_root`
   is the realistic case — a registered addons source's path is typically a
   subdirectory of the actual repo, and `git -C <subdir>` walking up to find
   it is real `git` behavior this module deliberately relies on rather than
   reimplementing; `rejects_a_directory_that_is_not_a_git_repo_at_all`,
   `rejects_a_repo_with_no_commits_yet` (a real, observed quirk: the command
   fails but still prints the literal string `"HEAD"` to stdout, which is
   exactly why this checks the exit code first), and `rejects_a_detached_head`
   (the one case where a *successful* command still has no branch name)
   round out the real failure modes.
2. **`Core`-level tests (5, against real git repos and real addons-source
   records)**: confirms a real linked private repo's real branch is
   returned; confirms a `Private` source that isn't a git repo, and the
   complete absence of a `Private` source, both collapse to `None` rather
   than an error; confirms an `Oca`-kind source is correctly *ignored* even
   when it genuinely is a git repo on a real branch (only `Private` counts);
   confirms an unknown server id is a real error, not a silent `None`.
3. **End-to-end against the real running API**: a `curl` pass — before any
   addons source is registered, `GET /servers/:id/git-branch` returns
   `{"branch": null}`; a real `git init`'d repo is checked out to
   `feature/new-invoicing`, its `addons/` subdirectory is registered as a
   `Private` addons source over the real API, and the same endpoint then
   returns `{"branch": "feature/new-invoicing"}` — resolved from the
   subdirectory path, exactly the shape a real addons source path takes.
4. **Browser-level (Playwright, `e2e_git_branch.mjs`)**: drove the real
   Topology screen's "Snapshot" button and captured the actual
   `window.prompt()` dialog's `defaultValue` — not just that a dialog
   appeared — confirming it was genuinely prefilled with the real linked
   repo's real branch name, then accepted that default and confirmed the
   resulting snapshot is actually named after the branch. All 4 assertions
   passed.

## Verifying the plugin architecture and Activity view

Task X.1's headline claim is that a plugin's declared permissions are
*enforced*, not documentation nobody checks — so the checks that matter
prove denial happens before any network call, not just that the UI looks
right:

1. **`scopedapi_test.mjs` (Node-level, real source, stubbed network)**: run
   via `npx vite-node ../scopedapi_test.mjs` from `frontend/` (plain `tsx`
   can't run it — `api.ts` reads `import.meta.env`, which only exists under
   Vite's module runner). Imports the actual `createScopedApi`/
   `createScopedSubscribeEvents`/`ScopeDeniedError` from `scopedApi.ts` with
   `fetch` stubbed to record calls instead of hitting a server. Confirms: a
   read-only-scoped plugin's `dropDatabase` call throws `ScopeDeniedError`
   naming `databases:write` and makes **zero** network calls; the same
   plugin's `listDatabases` succeeds; `revertDatabaseToSnapshot` (which
   needs both `databases:write` *and* `snapshots:write`) is denied when only
   one is granted and succeeds once both are; `subscribeEvents` is gated by
   `events:read`; and Activity's declared all-read scope set covers every
   call it actually makes. 8/8 checks passed.
2. **Browser-level, real running API, real UI (Playwright,
   `e2e_plugins_activity.mjs`)**: drives the real nav rail (now built from
   the plugin registry, not a hardcoded switch) through all four screens —
   creates and starts a real Postgres instance via Engine, creates a real
   server/database/snapshot via Topology, confirms Modules is still
   reachable and functional — all through screens that were refactored this
   task to receive `api`/`subscribeEvents` only via props, proving that
   refactor didn't regress anything already verified in earlier sessions.
   Then switches to Activity and asserts on the real rendered result:
   protection coverage genuinely reads "1 of 1"; the time-saved card's
   honest "not shown yet ... task 2.4" text is present; the event log shows
   real, human-readable descriptions built from names created in this exact
   run (`Created server "acme" (Odoo 17.0)`, `Created database "plugin_db"`,
   `Snapshotted "plugin_db" as "v1"`) rather than raw event-type strings; no
   points/badge/streak/leaderboard/XP language appears anywhere on the
   screen outside the one sentence that names those as deliberately absent;
   and the heatmap renders ~12 weeks of real day cells. 13/13 assertions
   passed. No leaked Postgres or shell processes after the run.

## Verifying reloading on save

Two switches per Odoo on the Config tab, both off until asked for, both
verified against a real Odoo 17 rather than against the source alone —
which is just as well, because the source reading was wrong twice.

1. **Unit (`odoo_conf.rs`, `lib.rs`)**: `collect_modules` maps a changed
   path to its module and refuses everything that shouldn't trigger an
   update — `.py` files (Odoo's own reloader has those), editor swap and
   autosave files, reads rather than writes, and paths outside every
   watched folder. `always_asks_odoo_to_route_by_the_first_label_of_the_host`
   pins the `dbfilter` line the generated config had been missing.

2. **`Core` level**: `turning_data_updates_on_starts_a_real_watcher_and_off_stops_it`
   asserts the setting *is* a live `notify` watcher in the handle map, not
   a row describing one — and that turning it off drops the handle, which
   is what actually stops it.
   `saving_a_view_under_a_watched_folder_really_reaches_us` writes a real
   file under a real watched folder and waits for the real event, closing
   the one gap the unit tests can't: that the watcher watches anything.
   `the_previewed_config_is_the_config_that_would_run` pins the config
   panel to the same renderer the start path uses.

3. **Real Odoo**: ran Odoo 17 four ways — with and without the setting in
   the config file, with and without `watchdog` installed, and with and
   without `dbfilter`. This is what produced the two corrections in
   `odoo-orchestrator-real-odoo-findings.md`: `dev_mode` in `odoo.conf` is
   read and discarded (it must be `--dev` on the command line), and the
   missing `dbfilter` meant every hostname reached the database picker
   instead of its database.

4. **Browser-level (Playwright, `e2e_reload_on_save.mjs`, 19 checks)**:
   clicks both switches in the real UI and follows each one down to the
   real command line and the real generated config — including that
   turning a switch back off removes the flag entirely, and that neither
   switch moves the other.

## The browser suites that were retired, and what covers them now

Six Playwright scripts were deleted rather than repaired:
`e2e_engine.mjs`, `e2e_snapshot.mjs`, `e2e_revert.mjs`,
`e2e_database_lifecycle.mjs`, `e2e_git_branch.mjs` and
`e2e_server_and_module_commands.mjs`, plus `e2e_plugins_activity.mjs`
before them.

They all drove Topology, Engine or Modules — three screens deleted in the
isolationist rebuild — so every one of them failed on its first locator.
Two of them also still pointed at `/Applications/Google Chrome.app`, so
they could not launch a browser in the first place. A test that cannot run
is worse than no test: the sections above cite these scripts as proof, and
that proof had quietly expired.

The write-ups above are left as they were, because they are an accurate
record of what was verified *at the time*. This section is the correction:
do not read those script names as current coverage.

What replaced them:

| Retired | Covered now by |
| --- | --- |
| `e2e_snapshot`, `e2e_revert` | `e2e_safety_net.mjs` — and more thoroughly: it writes a real row with `psql`, snapshots by clicking, deletes the row, restores by clicking, and reads the table back. The old pair asserted on dialog text and snapshot counts; neither ever checked that the data returned. |
| `e2e_database_lifecycle` | `e2e_safety_net.mjs` covers backup and restore end to end, including restoring into a *new* database and confirming the original is untouched. `e2e_four_screens.mjs` covers the Databases screen's listing, sizes and bulk actions. |
| `e2e_engine` | `e2e_four_screens.mjs` covers the Postgres store on Settings, where that screen's job moved. Multi-instance independence is proven at the Rust layer against real clusters (`two_distinct_postgres_instances_run_independently`). |
| `e2e_git_branch` | `Core`-level tests against real repositories, plus `snapshots_remember_their_branch_and_can_be_found_from_it`. The browser half needed out-of-band git fixture state and had already been failing to run for that reason. |
| `e2e_server_and_module_commands` | Real `odoo-bin` start/stop and real module install/upgrade/uninstall run at the `Core` layer against a real Odoo 17 checkout. **This is the one genuine reduction**: no browser test currently clicks Install. Said plainly rather than papered over. |

Two things the retired suites never covered, which the replacements do:
that restoring actually brings the data back, and that confirming a
destructive action closes its dialog. The second was a live bug — see
below.

## A bug the new suite found immediately

Confirming a destructive action on an Odoo left the modal open, with its
danger button still enabled, while the work it started ran behind it.
Closing the dialog was each caller's job; `Settings.tsx` did it and
`InstanceDetail.tsx`, `ProjectDetail.tsx`, `Activity.tsx` and
`Projects.tsx` did not. So the type-to-confirm guard on dropping a database
could be fired twice.

`ConfirmDialog` now closes itself on both paths — its prop is `onClose`,
not `onCancel`, and the confirm button calls it *before* invoking the
action. A caller can no longer forget. `e2e_safety_net.mjs` asserts the
dialog is gone after confirming.

## Verifying the safety net

`e2e_safety_net.mjs` is the only test in the project that checks the
product's actual promise rather than its plumbing. It writes a real row
with `psql`, snapshots by clicking, deletes the row, restores by clicking,
and reads the table back — then does the same round trip through a backup,
and again through the Activity log's "restore from its last backup".

The checks that exist because something was wrong:

- **The dialog closes on confirm.** It used to stay up with its danger
  button still live while the work ran behind it — a type-to-confirm guard
  you could fire twice.
- **A dropped database is still named in the log.** `DatabaseDropped`
  carried only an id, so Activity rendered `Dropped database "3f2a1b0c"`
  for the one row people go looking for after losing something.
- **An offer that can't complete isn't made.** A dropped database with no
  surviving backup gets no restore button; one with a backup does. Both
  directions are asserted, because only testing the positive would let the
  button appear unconditionally and pass.
- **Restoring never overwrites.** The restore panel refuses a name already
  in use and says why; Activity picks a free name itself rather than
  asking.

## Verifying major-version migration

The heaviest thing in the project, and the one where a mock would prove
nothing. `a_real_database_really_migrates_a_major_version` (opt-in via
`ORCHESTRATOR_TEST_REAL_UPGRADE=1`, because it downloads a second Odoo):

1. Fetches Odoo 16 and builds a **real 16.0 database**, asserting `base`
   really reads `16.0` before going further — a migration test that starts
   from the wrong version proves nothing.
2. Puts a recognisable row in it, so "the data survived" is a fact rather
   than an assumption.
3. Runs the migration to 17.0 through real
   `odoo-bin --update all --load=base,web,openupgrade_framework` with a
   real OpenUpgrade 17.0 checkout on the addons path.
4. Asserts `base` now reads `17.0`, the row is still there, **the original
   database is exactly as it was**, and the pre-hop checkpoint exists.

It passes: one hop, no failures, ~100s end to end including fetching Odoo
16.

Each hop is also verified independently of `odoo-bin`'s exit code, by
asking the database what version `base` reports — OpenUpgrade can exit 0
having logged a failure, and the exit code is not the fact anybody cares
about.

`e2e_migration.mjs` (16 checks) covers what a person meets before
committing: that the chain is shown as separate steps (15 → 18 is three
migrations, not one), that only newer versions are offered, that a
downgrade is refused with a reason rather than a 500, that each hop says
what it must download first, and that the copy's name is visible before the
button is pressed.

## Verifying the setup primitives

`e2e_fast_setup.mjs` (17 checks) covers the two features whose whole point
is that setup stops being slow and error-prone, and the `Core` tests cover
what a browser can't measure.

- **Instant databases.** `the_second_database_of_a_shape_is_a_copy_not_a_boot`
  builds one for real against Odoo 17 and then builds a second, asserting
  the second is at least four times faster; it printed 16.8s and 0.21s. It
  also checks both are working Odoo databases rather than fast empty ones,
  and that the cache isn't mistaken for a database somebody made.
  `clearing_the_cache_cannot_reach_a_database_somebody_made` puts a real
  database beside a template and clears the cache, because the decision to
  classify that as `system:write` rests on it being structurally unable to
  touch user data.
- **Is my code actually live.**
  `editing_a_module_without_touching_its_version_still_shows_as_changed`
  installs a module, edits its Python *without* bumping the manifest — the
  case version comparison cannot see — and asserts it reads as changed,
  then updates it and asserts it reads as live again.
  `a_module_installed_outside_this_app_reads_as_unknown` covers the third
  answer, which must never be drawn as "fine".
- **Finding addons folders.** `a_real_odoo_checkout_is_read_correctly` runs
  discovery against the actual Odoo 17 clone rather than a fixture — the
  shapes a hand-built fixture gets right are the ones you thought of. The
  browser suite then does it by clicking and checks the registered order.

## Repo layout

```
src-tauri/
  Cargo.toml                     workspace root (orchestrator-shell package)
  tauri.conf.json                Tauri v2 config, ready for when a window exists
  capabilities/default.json      Tauri v2 permission scope stub
  src/main.rs                    headless shell entry point
  crates/orchestrator-core/      domain model, SQLite store, event bus (no UI deps)
    src/manifest.rs                __manifest__.py parser (hand-rolled Python-literal subset)
    src/modules.rs                 addons scanning, shadowing detection, dependency graph
    src/postgres.rs                embedded-Postgres process supervisor (task 2.1)
    src/python_runtime.rs          uv-driven Python/venv provisioning (task 2.2, not yet wired into Core)
    src/odoo.rs                    generic Odoo/Python process supervisor (task 2.2, not yet wired into Core)
    src/pg_admin.rs                real Postgres admin ops: create/duplicate/drop/dump/restore (task 2.3)
    src/filestore.rs               hardlink-based directory snapshot primitive (task 3.1)
    src/git.rs                     real git-branch lookup (task 3.2)
                                    (revert_database_to_snapshot, task 3.3, lives in lib.rs alongside create/delete_snapshot)
  crates/orchestrator-api/       HTTP+WS API over the core (axum)
frontend/
  src/theme.css                  design tokens ported from the mockups (dark default, light override, accent hue 268°)
  src/lib/api.ts                 typed client + subscribeEvents()
  src/lib/scopes.ts              permission-scope taxonomy (task X.1)
  src/lib/scopedApi.ts           real, enforced scope checking wrapping api.ts (task X.1)
  src/lib/pluginTypes.ts         PluginManifest / PluginScreenProps shared types (task X.1)
  src/lib/plugins.ts             the PLUGINS registry — Topology/Engine/Modules/Activity (task X.1)
  src/App.tsx                    root shell, theme toggle, nav — nav/routing driven by PLUGINS (task X.1)
  src/screens/Topology.tsx       real screen — see "What's real" above (receives api via props)
  src/screens/Modules.tsx        real screen — addons sources + shadowing scan (receives api via props)
  src/screens/Engine.tsx         real screen — multi-instance Postgres list/create/start/stop (receives api via props)
  src/screens/Activity.tsx       real screen — event log + protection coverage + heatmap (task X.2)
  src/screens/StubScreen.tsx     unused for now; kept for the next genuinely-stubbed screen
scopedapi_test.mjs               real (stubbed-network) unit proof that scopes are enforced, not just declared
e2e_four_screens.mjs             Playwright: Databases/Stats/Activity/Settings against the real running API
e2e_workspace_flow.mjs           Playwright: project → Odoo → database → snapshot, entirely by clicking
e2e_reload_on_save.mjs           Playwright: the two reload switches, down to the real odoo-bin command line
e2e_safety_net.mjs               Playwright: snapshot → lose the data → restore → check Postgres; the friction ladder; backup round trip
e2e_fast_setup.mjs               Playwright: instant databases and addons discovery, against the real Odoo checkout
e2e_migration.mjs                Playwright: the migration chain, what it shows before you commit to it
```

## What's next

See `odoo-orchestrator-task-breakdown.md` for the full breakdown. Workstream
1 (manifest parsing, shadowing detection, dependency graph — tasks 1.1
through 1.5) is done end-to-end, real filesystem to real UI. Task 2.1's
Postgres process supervisor — including the multi-instance decision and the
full `Core`/API/frontend wiring — is done end-to-end too. Task 2.3's database
lifecycle (create/duplicate/drop/backup/restore) is also done end-to-end,
verified against real Postgres at the unit, `Core`, HTTP-API, and browser-UI
layers — and it resolved 2.2's "which `PostgresInstance` does a server use"
open item along the way. **Workstream 3 (branch-based snapshotting) is now
fully done**: task 3.1's snapshot primitive (Postgres database duplicate +
optional hardlinked filestore copy, combined into one named, point-in-time-
frozen artifact), tasks 3.3/3.4's one-click revert (overwriting a live
database in place from an existing snapshot, with a real counter-snapshot
and real friction-ladder confirmation copy), and task 3.2's git-branch
binding (the Snapshot prompt prefilled with a real repo's real current
branch) are all done, verified against real Postgres, a real filesystem, a
real `git` binary, the real HTTP API, and a real browser at every layer (see
"Verifying the snapshot primitive," "Verifying one-click revert," and
"Verifying git-branch binding" above). 3.2 turned out not to be blocked on
anything this sandbox lacked — an earlier version of this doc conflated it
with 2.2's genuinely-still-blocked need for a real *published* (GitHub)
Odoo checkout, when it only ever needed the local `git` binary and a
locally-creatable repository.

**Tasks 2.2 and 2.4 are now also done, including their `Core`/API wiring —
finished on a real local desktop with a real Odoo 17.0 checkout already on
disk**, picking up exactly where this file's own earlier version (and
`CONTINUE_LOCALLY.md`) left off: a cloud sandbox's egress policy blocked
GitHub, the only place real Odoo source is distributed from, so 2.2's two
generic building blocks (Python/venv provisioning via `uv`, and a
foreground-process supervisor with dual-stream log capture and immediate
crash detection) had been built and tested standalone but deliberately not
wired into `Core` or run against real `odoo-bin`. That wiring is now real:
a new `odoo_conf` module renders a real `odoo.conf` from a server's
registered addons sources and its running `PostgresInstance`;
`Core::start_server`/`stop_server` spawn and supervise real `odoo-bin`,
with server state persisted to SQLite on every transition; and 2.4's
`Core::install_modules`/`upgrade_modules`/`uninstall_modules` run real
one-shot `odoo-bin -i`/`-u`/`shell` commands per database, with a real
post-upgrade module version read back from `ir_module_module` rather than
fabricated. Along the way, three real Odoo/Postgres compatibility issues
were found and fixed, not glossed over: Odoo refuses to start under the
Postgres superuser role (a dedicated non-superuser `odoo` role is created
instead); recent `setuptools` releases dropped `pkg_resources`, which
Odoo's own code imports directly (the venv pins `setuptools<81`); and
Postgres 15+ revoked the default `CREATE`-on-`public`-schema privilege
every earlier version granted (an explicit per-database grant fixes it).
Verified end-to-end against real `odoo-bin`: a real server started and
confirmed serving HTTP, and a real module installed, upgraded (with a real
version reported), and uninstalled — each confirmed against
`ir_module_module`'s actual state, not just a claimed success. See
`odoo-orchestrator-task-breakdown.md`'s 2.2/2.4 entries for the full
verification detail.

**Workstream 2 is now fully done, including 2.5's frontend half.**
`Topology.tsx` has real server Start/Stop controls; `Modules.tsx` has real
Install/Upgrade/Uninstall buttons per module (with a per-database picker,
since module state is per-database) and the real "N up to date" instrument
the Modules screen had been deferring since task 1.5 — computed by
comparing each scanned module's manifest version against its database's
real `ir_module_module` version, using Odoo's own real version-adaptation
rule (replicated client-side, not guessed — see "Verifying real Odoo
server supervision and module commands" below for why a naive comparison
would have been wrong). Both are backed by a shared `OdooRuntimeSettings`
panel (checkout root, provisioned venv, runtime dir — entered once,
persisted in `localStorage`). Verified end-to-end against a real running
server and real `odoo-bin`, in a real browser — see that same section.

**X.1 (plugin manifest + contribution-point loader) and X.2 (event log
persistence + Activity view) are now also done.** X.1 built a real,
*enforced* permission-scope system (`scopedApi.ts`) and a plugin registry
(`plugins.ts`) that `App.tsx`'s nav rail and screen routing are now derived
from, rather than a hardcoded `Screen` union — Topology, Engine, and
Modules were refactored to receive their API access only through props
(never importing the `api` singleton directly), so their declared
permissions are genuinely the only thing they can do, not documentation.
X.2 added Activity, a new screen registered purely by one entry in the
plugin registry — proof the loader works for a screen it didn't ship with
— showing real "protection coverage," a real no-streak activity heatmap,
and human-readable descriptions for the full real event catalog, with an
honest "not shown yet" card in place of a fabricated time-saved number.
Both verified against the real running API and a real browser (see
"Verifying the plugin architecture and Activity view" above), plus a
Node-level scope-enforcement proof (`scopedapi_test.mjs`) run with a
stubbed network to confirm denial happens before any request is made.

**What's still genuinely blocked**: only 0.6/0.7, all of Workstream 4, and
X.3/X.4 — every one of them needs a real desktop environment for Tauri's
webview/sidecar mechanism specifically (a buildable, installable Tauri
app, not just "a machine with Odoo on it," which is what unblocked all of
Workstream 2 above). This machine has confirmed `cargo`/`rustc` and Xcode
Command Line Tools, so that track is buildable here too — it just hasn't
been picked up yet. Absent that, there is currently no further buildable
slice of this project left.
