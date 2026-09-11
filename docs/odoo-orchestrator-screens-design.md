# Activity, Stats, Settings, Databases — what each should hold

A design pass before building. Every element below is checked against data
that actually exists today; anything that would need new instrumentation is
marked, rather than being drawn and quietly faked later.

The nav becomes five: **Projects · Databases · Activity · Stats · Settings.**

What already exists to build on: 27 event kinds, all timestamped with typed
payloads; real database sizes; `last_backup_at`; snapshot rows with
filestore paths; server state and `started_at`; module scans with collision
detection; the Odoo runtime cache with its source (downloaded vs. adopted);
the routing table; and `pg_admin::query_rows`, which can ask a real cluster
what databases it actually has.

---

## 1. Activity — from a feed into something you act on

The current screen is a flat list. A list of things that happened is not
useful on its own; three specific questions are.

### "What happened while I was away?"

Parnin's resumption research (10–15 minutes to rebuild context after an
interruption) is already cited in the design principles. A **"since you
last had this open" marker** in the timeline collapses that into a glance.
Cheap: store the last-seen timestamp per viewer, draw a rule above it.

### "What touched a client's data?"

A **destructive lane** — drops, reverts, deletes get their own visual
treatment, because that's what you actually scan a log for. Filter chips:
`All · Databases · Modules · Snapshots · Destructive`, plus a scope filter
for one project.

### "Can I undo that?"

The part that makes this more than a log. Some events have a **real
inverse**, and the row should offer it:

| Event | Inverse offered | Where it comes from |
|---|---|---|
| `database_reverted_to_snapshot` | **Undo this restore** | the `counter_snapshot_id` already in the payload |
| `database_backed_up` | **Restore this backup** | the dump path already recorded |
| `snapshot_created` | **Restore it** | the snapshot row |
| `database_dropped` | Restore from its last backup, *if one exists* | backup events for that name |
| `module_installed` | Uninstall it | module commands |

Everything else offers nothing, and says so by simply having no button.
This is the reversibility thesis made concrete: the safety net is visible
at the moment you're looking at the thing that scared you.

**Shape:** grouped by day (`Today`, `Yesterday`, `Tue 26 Aug`), each row
carrying its object as a breadcrumb (project → Odoo → database) so an entry
is meaningful without hunting. Consecutive identical entries collapse to
`×N` (already built). The heatmap stays — still no streak counter.

---

## 2. Stats — instruments, and only ones that are true

### Where the disk went

The single most useful number a local dev tool can show, and nobody shows
it. One bar, broken down, each row reclaimable:

- databases (real sizes, per project)
- snapshots
- backups on disk
- **Odoo runtime cache, per version** — often the biggest and most
  surprising item, and the easiest to reclaim
- filestores

"40 GB, and 22 of it is three copies of Odoo you're not using" is a fact
someone acts on immediately.

### Protection

`N of M databases have a snapshot`, the oldest unprotected one and how long
it's been that way, median snapshot age. Loss aversion pointed at a real
database, dischargeable in one click. Already the instrument used on
project cards — this is the whole-estate view of it.

### Speed, as arithmetic — not a score

The design doc's "time-saved as arithmetic" instrument, computed from
**real event timestamps** by pairing start/finish events:

> Restoring a snapshot takes about **8s**. The last time you rebuilt this
> database from scratch it took **4m 10s**.

Median time to create a database, to restore, to update modules. Facts with
their own sample size shown (`median of 14`), never a headline number with
no provenance. Needs one small addition: duration on the completion events.

### The rest

- **Running now** — which Odoos are up and for how long. *(Memory and CPU
  per process are not measured today; either instrument them properly or
  don't show them.)*
- **Versions in use** — which Odoo versions across all projects, which are
  cached, which were adopted from a checkout already on the machine.
- **Module health across everything** — total modules, how many are
  shadowed by a copy in another folder, unresolved dependencies. All real
  from the existing scan.

No score. No grade. No ranking of anything.

---

## 3. Settings — the things that are genuinely settings

Everything here is either already a real value or something the app must
decide anyway.

**Appearance** — theme. (Move it out of the nav rail; it isn't navigation.)

**Addresses** — proxy port; the suffix (`.localhost` default, `.test`
offered, custom allowed); a *test this address* button that actually makes
the request and reports what happened. Carries the honest note that Safari
on macOS doesn't resolve `*.localhost` subdomains, and links the reasoning
in `odoo-orchestrator-address-decision.md`.

**Where databases are kept** — the list, add, start/stop, disk used per
store. This is the old Setup screen's first half, correctly filed as a
setting rather than a destination.

**Odoo copies** — the runtime cache: version, size, where it came from
(downloaded / adopted from `~/odoo-dev`), path, delete. Plus "also look in
these folders" for adoption. This is what makes the cache legible instead
of a black box that silently eats gigabytes.

**Defaults for new Odoos** — default version; **auto-snapshot before
anything destructive** (on by default — this is the friction ladder's
pre-checked box promoted to a policy); where backups are written.

**Danger zone** — clear the runtime cache; reset app state. Type-to-confirm,
rung 4.

---

## 4. Databases — one place for all of them

Projects answer "what am I working on". This answers "what data exists on
this machine", which is a different question and currently unanswerable
without clicking through every project.

**One table across everything:** name · project · Odoo (version) · address ·
size · protection · last backup · state. Sortable, searchable, filterable
(unprotected only; by project; by version).

**Bulk actions** are the point. Select several → **back up** or **snapshot**
them. "Eight client databases and I'm about to do something scary" is a
real Monday morning, and doing it one at a time is why people don't.

**Reconcile with reality.** The app's rows and the actual cluster can drift,
and pretending otherwise is how a tool loses trust. Compare against
`SELECT datname FROM pg_database` and show both directions:

- **Untracked** — real databases the app doesn't know about (from an older
  workflow, or made by hand). Offer to adopt them.
- **Missing** — rows whose database is gone underneath. Offer to clean up
  the row.

Both are honest, both are real, and both use machinery that already exists.

---

## What this needed that didn't exist yet — all four now built

1. ✅ **Duration on completion events.** `EventEnvelope.duration_ms`, on the
   envelope rather than added to two dozen event variants: it's a property
   of *recording* an event, not of what happened. Six operations are timed
   (create, duplicate, back up, restore, snapshot, revert), with a schema
   column, an idempotent migration, and persist/read-back.
2. ✅ **Last-seen timestamp per viewer.** localStorage, read into state once
   on mount so looking at the screen doesn't erase the line it draws.
3. ✅ **`list_cluster_databases`** — one `pg_database` query with real
   `pg_database_size`, excluding template and maintenance databases.
   `reconcile_databases` builds on it, plus `adopt_database` and
   `forget_database` as the two fixes.
4. ✅ **Directory sizes** — `odoo_runtime::directory_size`, feeding
   `Core::disk_usage`.

---

## What got built, and the decisions taken along the way

**Nav is now Projects · Databases · Activity · Stats · Settings**, and the
theme switch left the rail — it isn't navigation.

### Honesty rules that shaped the code

- **"I couldn't look" is never reported as "it's gone."** A database store
  that isn't running is listed as `unchecked`; its databases are
  deliberately *not* reported as missing. Sending someone to a backup they
  don't need is its own kind of data loss. There's a test for it.
- **`forget_database` and `drop_database` are separate calls, and the API
  route for forgetting is a POST**, so it can't be confused with the DELETE
  that really destroys data.
- **Snapshot databases are excluded from "untracked"** — they're real
  databases the app made on purpose, and listing them as strays would train
  people to ignore the screen.
- **Sizes are measured on demand, not live.** `pg_database_size` is a real
  query per database; running one on every list would make the screen
  slower the more work someone has. The UI says when it last measured.
- **Undo is offered only where a real inverse exists** and can actually
  complete. `database_backed_up` records that a dump happened but not its
  path, so it gets no button — an offer that fails when pressed is worse
  than no offer.
- **Stats shows only measured facts.** No memory or CPU per process,
  because neither is instrumented. Every median carries its sample size.
  Versions in use are checked with a real `find_odoo` lookup rather than
  assuming a copy is present.
- **The cache refuses to delete anything outside itself.** "Clear the
  cache" can never eat somebody's own `~/odoo-dev`; there's a test that
  points it at a sibling checkout and asserts survival.

### The address suffix became real

Settings offers `localhost` (default, nothing to install) or `test`. That
would have been a lie while the proxy matched the whole hostname, so
`find_route` now matches a **default** address on its first label — the
same thing Odoo's own dbfilter does — while a database given a **custom**
domain is still matched exactly. Handing an unasked-for second address to
the one database whose owner explicitly chose otherwise would be exactly
backwards. Four tests cover it.

### Permission scopes

Two new ones, `system:read` and `system:write`, so seeing that three copies
of Odoo eat 3 GB is a different power from deleting one. Settings is the
only holder of `system:write` and notably has **no** `databases:write` — no
setting should ever be able to touch a database. Stats is entirely
read-only; Activity holds the two snapshot scopes solely for its inline
undo.

### Proof

118 core tests (against real PostgreSQL) and 9 API tests pass, including
eight new ones for reconciliation, sizes, directory measurement and cache
safety. `e2e_four_screens.mjs` drives the real UI in a real browser against
the real backend: 26 checks, all passing — including that measuring writes
a genuinely non-zero size, that the coverage figure matches what the API
itself reports, and that no request fails anywhere in the run.
