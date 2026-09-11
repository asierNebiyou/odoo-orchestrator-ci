# What's left to build (Sept 2026)

> **Status — everything buildable from here is done.** Sections 1 and 2
> are complete, and section 4's two real items with them: backups now
> carry the filestore, and memory/CPU per process is instrumented rather
> than absent. 129 core tests, 9 API tests, both browser suites.
>
> **What is left needs a macOS terminal** — section 3, the desktop shell.
> Nothing else in this document can be built or verified from a cloud
> sandbox.

Everything below was checked against the code this session, not carried
forward from an earlier list. Each item says what it protects, what already
exists to build on, and roughly what it costs.

---

## 1. Correctness — the product's own promises. All four now kept.

This section listed four places where the app told the user something
untrue. All four are closed. They are kept here rather than deleted,
because the pattern they share is the useful part: **every one of them was
a feature that looked finished from the outside.**

### 1.1 Snapshots ignored the filestore — **done**

Every snapshot was database-only, so restoring to last Tuesday brought the
rows back and left the invoice PDFs behind. For a tool selling "restore
without fear", silently restoring half the data is the worst kind of bug:
it looks like it worked.

Fixed by teaching `Core` to *derive* the filestore path
(`<app_dir>/run/<server_id>/filestore/<dbname>`) and pass it on both sides,
rather than leaving it to a caller that never passed it. The pattern worth
remembering: the app was demanding knowledge only the app could have.

### 1.2 Dropping a database orphaned its filestore — **done**

`drop_database` now removes it, after the database is really gone, and
non-fatally — a failure to tidy up must not make the caller think the drop
failed.

### 1.3 The auto-snapshot setting did nothing — **done**

Settings wrote `orchestrator:autoSnapshotBeforeDestructive` and nothing
read it. `ConfirmDialog` reads it now, so the pre-checked safety snapshot
reflects the setting instead of being hardcoded.

### 1.4 A backup recorded that it happened, not where it went — **done, and then some**

The event now carries `path` *and* `server_id`, which turned out to unlock
more than the original note anticipated:

- `Core::list_backups` reads the backups folder, so the app can find its
  own dumps again. Before this, `restore_database` existed at every layer
  and no screen called it — "Back up" produced a file only a terminal
  could use.
- The Odoo's Databases tab has a restore panel. It always creates a *new*
  database and refuses a name already in use, so no path through it
  overwrites anything.
- Activity offers "Restore this backup" on a backup row, and "Restore from
  its last backup" on a *dropped* row — the inverse that section 1.4 said
  was impossible.

`DatabaseDropped` also carries the name and server now. It used to carry
only an id, so the log rendered `Dropped database "3f2a1b0c"` — accurate,
useless, and the exact row somebody goes looking for after a bad
afternoon. Nothing else remembers a deleted database's name.

## 2. Robustness — the same class of bug. Both closed.

### 2.1 `odoo-bin` leaks exactly like PostgreSQL did (M) — **done**

`OdooSupervisor` has no `Drop`, and its watchers hold strong clones of
themselves — **the identical bug just fixed in `PgSupervisor`**, still
present for Odoo. Quit the app without a clean shutdown and its Odoo
processes keep running, holding their HTTP ports.

Fixed by carrying the PostgreSQL fix across: a `StopOnDrop` guard in its
own `Arc` (TERM, a bounded grace period, then KILL), and a `TaskHandle`
for the log reader and wait task so neither keeps the process alive by
watching it.

**The adopt-on-start half was deliberately *not* copied.** A PostgreSQL
data directory identifies its own postmaster, so adopting one there is
provably reclaiming our own process. A TCP port proves nothing — the
listener could be an Odoo this app forgot, a second copy of the app, or
something unrelated — and claiming a stranger's process as ours would be
worse than failing. So `start()` reports a distinct `PortInUse` error
(409, not 500) naming the port, instead of spawning a process that dies on
bind and surfaces as a confusing startup timeout.

### 2.2 Graceful stop is Unix-only (S) — **done, but unverified**

`odoo.rs` used to log *"isn't implemented on this platform yet"* and kill
nothing at all, which meant a Windows user's Odoo processes survived every
stop. Now implemented with `taskkill` (without `/F` to post WM_CLOSE, with
`/F` to terminate) and `tasklist` for the liveness probe, mapping exactly
onto the TERM-then-KILL escalation the Unix path uses.

**Written from the documented behaviour, never run on Windows** — nothing
in this project has. Worth knowing before trusting it; it is a real
implementation replacing a no-op, not a verified one.

---

## 3. The desktop shell — one blocker, four tasks behind it

None of this can be built or verified from a cloud sandbox: it needs a real
desktop with a webview. **This is the only track that needs Asi at a macOS
terminal**, and it gates the rest.

### 3.1 Build and run the Tauri app at all (M) — *nothing else here starts until this does*

`npm run tauri dev`. The app has never been launched as a desktop app; every
verification so far has been a headless backend plus a browser. Unknowns
this immediately settles: whether the sidecar lifecycle handles a
long-running PostgreSQL cleanly (start, health-check, stop on quit,
recovery from a mid-session crash), and whether the per-launch token
handshake works through the real shell rather than `.env.local`.

### 3.2 The editor panel (L)

`openvscode-server` as a supervised sidecar, pointed at this Odoo's code and
aware of which database it's hitting. Currently an honest "isn't wired up
yet" card that names its own blocker.

### 3.3 The built-in browser panel (M)

Odoo in a panel beside the code. Same blocker, same honest card.

### 3.4 Updater and bundler, and a multi-webview smoke test (M)

Both need a buildable, installable app — i.e. 3.1 first.

---

## 4. Instruments that are deliberately absent, and could stop being

Each of these is currently *missing on purpose* rather than faked, and the
UI says so. Building them is optional; faking them is not.

- ~~**Memory and CPU per Odoo process**~~ — **done.** Read from the OS via
  `ps` (rss + pcpu), exposed as `/process-usage`, shown beside uptime on
  Stats. An Odoo this app isn't supervising still shows nothing rather
  than a zero, and the screen says why.
- ~~**A "zip" backup format**~~ — **done**, though not as a zip: the dump
  is written as before and the filestore beside it as
  `<dump>.filestore/`, so the two travel together and either can be
  inspected without unpacking anything. Copied rather than hardlinked,
  because a backup has to survive being moved to another disk or kept
  after the original is deleted — the opposite of what a snapshot wants.
  `restore_database` puts it back under the new database's name.
- **Module scan caching** — scans walk the filesystem on every request.
  Fine at current scale; revisit only if a large addons path is *observed*
  to be slow, not on suspicion.
- **Opt-in pretty addresses** (`.test` with a resolver entry, no port) —
  designed in `odoo-orchestrator-address-decision.md`, deliberately not
  built: it needs privileged setup, so it must be an explicit action that
  states exactly what it changes and can undo all of it.

## Reloading on save — built, with two corrections from real Odoo

Two switches per Odoo, off by default, on the Config tab:

- **Restart when a `.py` file changes** — delegated to Odoo's own
  `--dev=reload`, which compiles a change before acting on it, so a syntax
  error lands in the log instead of restarting into a broken server.
  Turning it on installs `watchdog` into that Odoo's interpreter, because
  the flag silently does nothing without a watcher module. Applies at the
  Odoo's next start, and the UI says so.
- **Update the module when its data files change** — this app's own
  `notify` watcher over the non-core addons folders, debounced 1.2s, which
  runs `-u <module>` only on databases where that module is actually
  installed. This one exists because Odoo has no mechanism for it at all:
  a changed view or data CSV takes effect only after an update, and
  nothing in Odoo watches for that.

Two things real Odoo corrected, both written up in
`odoo-orchestrator-real-odoo-findings.md`:

1. `dev_mode` in `odoo.conf` is **ignored** — Odoo recomputes it from the
   command-line `--dev` after reading the file. The first implementation
   wrote it to the config and did nothing.
2. The generated config **never emitted `dbfilter = ^%d$`**, so every
   address on the port reached the database picker instead of its
   database. The UI hid this by drawing its config panel from a
   hand-written approximation that did include the line. That panel now
   fetches the real rendered config *and* the real command line.

## Backups became restorable

`backup_database` had been writing dumps that nothing in the app could ever
find again. `restore_database` existed at every layer — core, API, typed
client — and no screen called it, because a restore needs a file path and
nothing listed the files. So "Back up" was a button that produced a thing
you could only use by opening a terminal.

Closed with `Core::list_backups`, which reads the folder rather than a
table (the dumps are plain files a person may move or delete outside the
app, and a list built from a table would confidently name files that are
gone), a `GET /backups` route, and a panel in the Odoo's Databases tab.
Restoring always creates a *new* database and refuses a name already in
use, so there is no path through it that overwrites anything.

Found while auditing what the retired browser suites had covered — the
old `e2e_database_lifecycle.mjs` drove a Restore button on a screen that
no longer exists, and nothing had noticed the button never came back.

## The four `click-odoo-contrib`-class primitives, built

The research doc named a GUI over `click-odoo-contrib`'s primitives as one
of six unclaimed differentiators. Four of them exist now.

## Major-version migration, with the chain wrapped

The research doc's fifth differentiator, and the one it described as having
"no orchestration wrapper" anywhere. Built.

**What OpenUpgrade actually is**, confirmed against the real repository
rather than from memory: one repository with a branch per *target* version
(5.0 through 19.0 exist), where branch `17.0` migrates a 16.0 database to
17.0. Since 14.0 each branch ships two Odoo modules at its root,
`openupgrade_framework` and `openupgrade_scripts`, so the repository root
goes on the addons path. `openupgrade_framework` must be loaded
**server-wide** (`--load=base,web,openupgrade_framework`) because it patches
Odoo's module loading before the registry is built. Its one Python
dependency is `openupgradelib`, installed from git per OCA's own
instructions.

**You cannot skip a version.** 15.0 → 18.0 is three separate migrations, in
order. That single fact is why this feature is worth building: nothing
wraps the chain, so a three-hop migration that fails on hop three leaves
you between two versions with whatever you remembered to copy first.

The wrapper:

- **Works on a copy.** The database you pick is duplicated and the
  migration runs on the duplicate. The original is never opened for
  writing — that is the whole reason to use this rather than running
  OpenUpgrade by hand.
- **Checkpoints before every hop.** A real snapshot, named for the hop, so
  a hop that dies half way through has a way back.
- **Stops at the first failure and says where.** A partial run is a normal
  outcome, not an error: the hops before it really did succeed, and the
  screen names the one that stopped, the checkpoint to return to, and the
  fact that the original is untouched.
- **Says what it must download before starting.** Each hop reports whether
  its Odoo and its OpenUpgrade branch are already on disk.
- **Trusts the database, not the exit code.** OpenUpgrade can exit 0 having
  logged a failure, so each hop is verified by asking the database what
  version `base` now reports.

**Verified for real**, not mocked: `a_real_database_really_migrates_a_major_version`
fetches Odoo 16, builds a real 16.0 database, puts a recognisable row in it,
migrates it to 17.0 through real `odoo-bin --update all`, and then checks
that `base` reads 17.0, that the row survived, that the original database
is byte-for-byte where it was, and that the checkpoint exists. It is opt-in
(`ORCHESTRATOR_TEST_REAL_UPGRADE=1`) because it downloads a second Odoo.


### Neutralization on restore

Covered in full in `odoo-orchestrator-real-odoo-findings.md`. The short
version: restoring a client's dump used to leave their outgoing mail
servers active *with working passwords*, every cron running, and live
payment credentials in place. It now runs Odoo's own neutralization by
default, and reads the resulting state back out of `ir_config_parameter`
rather than remembering having done it.

### Instant databases

`create_database` was a bare `CREATE DATABASE`, so a "new database" was not
an Odoo database at all — you then went to Apps, installed something, and
waited out a full `-i` boot. Every time.

Now the first database of a given shape pays that boot and is kept as a
template; every later one is `CREATE DATABASE ... TEMPLATE`. Measured
against real Odoo 17: **16.8s cold, 0.21s warm** — and that is with only
`base`; a real project's module set makes the cold path minutes.

The cache key is (Odoo version, module set, **content hash of those
modules**), so editing a module invalidates every template built from it.
Settings shows the cache and can clear it — classified `system:write`
rather than `databases:write`, which is only honest because
`clear_template_cache` can structurally reach nothing but the names it
built itself. There is a test that proves that rather than asserting it.

### Only updating what changed

Two problems, both fixed:

- **Staleness was guessed from manifest versions**, which only notices a
  change when somebody remembered to bump one — and nobody bumps it while
  iterating. It's a content hash per (database, module) now, recorded at
  install and update time, so an edit shows immediately. A module installed
  before this app started recording reads as **unrecorded**, not "fine".
- **"Update all" ran one `-u` per module**, and Odoo reloads its whole
  registry on each of those. It's one batched `-u a,b,c` now, falling back
  to one at a time only when the batch fails — so a failure still names the
  module that caused it, without paying for that on every success.

### Finding addons folders

Registering module folders meant typing each path and a priority number by
hand, and a wrong order silently changes which copy of a module Odoo loads.
Point at a folder now and it finds every addons root under it, proposes the
Doodba order (private > OCA > core), and **states its reason for every
guess** — because a guess nobody can check is one they have to trust.

This also closed a real gap: a checkout's `odoo/addons` (where `base`,
`web` and `bus` live) was never registered, so those modules reported as
"not on disk". Discovery finds both folders.

---

## Suggested order

Sections 1 and 2 are done. What genuinely remains:

1. **3.1 run the desktop app** — the largest unknown left in the project,
   and the only one nobody can answer from a cloud sandbox. Everything in
   section 3 is behind it.
2. **A browser test that clicks Install** — the one place browser coverage
   genuinely shrank when the stale suites were retired. Real module
   installs are still proven at the `Core` layer against a real Odoo 17,
   so this is a gap in *where* it's proven, not whether.
3. **Migrating a database with real client modules in it.** The migration
   chain is built and proven on a `base`-only database. A real client
   database carries custom modules written for the old version, and those
   are the most likely thing to break a hop. The migration deliberately
   leaves the server's own addons sources off the addons path — that is a
   defensible default, but it means a custom module is *not* migrated, only
   Odoo's own. What should happen to custom modules is a product question
   worth answering deliberately rather than by default.
4. **2.2's Windows path** — implemented from the documented behaviour and
   never run on Windows. Worth knowing before trusting it.
5. Section 4's two remaining optional instruments, on evidence rather than
   suspicion.

All six of the research doc's candidate differentiators are now built
except the embedded editor, which is behind the desktop shell.
