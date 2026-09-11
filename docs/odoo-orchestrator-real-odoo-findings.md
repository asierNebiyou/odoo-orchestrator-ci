# Running it against real Odoo in the cloud sandbox (Sept 2026)

**Two real bugs found and fixed: every database was created with the
machine's locale encoding (Odoo cannot use a non-UTF8 one), and dropping
the PostgreSQL supervisor left its server running.** One claimed finding
turned out to be wrong on closer inspection and is recorded as such.

Tasks 2.2/2.4/2.5 were completed and verified on a real local desktop, with
a real Odoo checkout. In the **cloud sandbox**, though, the two tests that
prove it — `start_server_runs_a_real_odoo_bin_and_serves_http` and
`install_upgrade_and_uninstall_a_real_module_against_a_real_database` —
have been **skipping**, not passing. Both guard on
`ORCHESTRATOR_TEST_ODOO_CHECKOUT`, and with no checkout present they return
early: they print a skip line, but still count as `ok` in the summary. So
every "118 tests pass" reported from a sandbox session has been silent on
the product's most central claim.

This session cloned real Odoo 17.0 into the sandbox and ran both for real.
That difference in environment is not incidental — it is what surfaced the
bug below, which cannot reproduce on a normal Mac.

## The sandbox is no longer cut off from GitHub

Earlier sessions recorded that the sandbox's egress policy blocked GitHub —
the only place real Odoo source is distributed from — which is why that
work had to move to a local desktop. **That is no longer true**:
`git clone https://github.com/odoo/odoo` succeeds from the sandbox now.

Worth knowing, because it means the sandbox can once again verify the
Odoo-dependent half of the product itself, instead of handing it off. It
also means the two environments now genuinely differ only in locale and
desktop capability — which is exactly the gap that hid the bug below.

## The real bug this found

**Every database the app created was unusable by Odoo on any machine whose
locale isn't UTF-8.**

`initdb` was invoked with no `--encoding`, so it inherits the machine's
locale. Where that locale is unset or `C`, PostgreSQL produces a
`SQL_ASCII` cluster. Odoo then fails to initialize *any* database on it —
its very first insert into `ir_module_module` carries a `®` (the
`delivery_mondialrelay` module's summary):

```
psycopg2.errors.UntranslatableCharacter: unsupported Unicode escape sequence
DETAIL: Unicode escape value could not be translated to the server's
        encoding SQL_ASCII.
CONTEXT: JSON data, line 1: ...choose a Point Relais®...
```

The failure mode is the worst kind: **environment-dependent**. A typical Mac
exports `LANG=en_US.UTF-8`, so this works there and fails on a colleague's
machine, a CI box, or any user whose shell doesn't. That is precisely what
happened here: the same code passed on the local desktop where 2.4 was
originally verified, and failed the first time it met a machine with a
bare locale. It would have shipped looking fine.

Fixed in two places, deliberately both:

- `postgres.rs` — `initdb --encoding=UTF8 --locale=C.UTF-8`, so every
  cluster this app creates is identical regardless of who is logged in.
- `pg_admin.rs` — `createdb --encoding=UTF8 --template=template0
  --lc-collate=C --lc-ctype=C`, for clusters the app *didn't* create (an
  adopted one, or an existing local PostgreSQL) whose `template1` may not
  be UTF8. Naming `template0` is what makes an explicit encoding legal;
  `createdb` refuses `--encoding` against `template1` unless they agree.

Guarded by a new test that asserts the property directly —
`every_database_this_app_makes_is_utf8_whatever_the_machine_locale` — rather
than relying on the real-Odoo tests, which only run when someone has a
checkout to point at.

## The second bug, also fixed: a leaked PostgreSQL

**Dropping the supervisor left its PostgreSQL running.** A test that
panicked before its explicit `stop_postgres_instance` left a postmaster
holding its port, and every later run collided with it — which is how the
encoding investigation above kept failing for the wrong reason. The same
thing happens to a user: quit the app without a clean shutdown and its
PostgreSQL is still there, holding a port and a data directory that
nothing is tracking any more.

Fixing it turned up a second layer. A naive `Drop` on `PgSupervisor` would
never have fired, because **the background health watcher held a strong
clone of the supervisor** — so the last handle could never be last. The
fix is therefore two things:

- `StopOnDrop`, held as its own `Arc` inside the supervisor so it runs once
  when every clone is gone, issuing a best-effort synchronous `pg_ctl stop
  -m fast`.
- The health watcher now holds a **weak** reference and its own
  `WatcherHandle` (state and config, nothing that keeps the cluster
  alive). Watching something must not be what keeps it running.

And for the case a `Drop` can never cover — a hard kill, where no Rust
code runs at all — `start()` now **adopts** a postmaster already serving
its data directory instead of failing with "address already in use". It is
already the thing we were about to create.

Two tests cover it: `dropping_the_last_handle_stops_the_cluster` (and
asserts that dropping a *clone* does not) and
`starting_over_a_cluster_left_running_by_a_killed_app_adopts_it`. After a
full suite run, zero stray postmasters remain.

## A finding that turned out to be wrong

An earlier draft of this document claimed the app needs system build
headers (`libldap2-dev`, `libsasl2-dev`, `libpq-dev`) to provision a venv,
because installing Odoo's `requirements.txt` by hand fails without them —
`python-ldap` and `psycopg2` compile from source.

That is true of the **naive** path, and it is what happened when this
session installed the requirements manually. It is **not** true of the
app's own path: `adjust_requirements_for_this_platform` already drops
`python-ldap` (no wheels anywhere; Odoo excludes it on Windows itself) and
substitutes `psycopg2-binary` for `psycopg2`, so neither package is ever
compiled. Verified by running the real-Odoo test with no pre-provisioned
venv, so `ensure_python_installed` → `ensure_venv` →
`install_requirements` all ran for real: it passes.

Recorded rather than quietly deleted, because "the tool needs build
headers" is the kind of claim that would have propagated into install
docs and support answers and been wrong in both.

## What this cost, for the next person

- Clone: real Odoo 17.0, shallow, single-branch — **1.2 GB**.
- Dependencies: `uv venv --python 3.11` plus the full `requirements.txt`.
- Both real tests then run in about **19s** and **6s** respectively.

To run them:

```
export ORCHESTRATOR_TEST_ODOO_CHECKOUT=/path/to/odoo
export ORCHESTRATOR_TEST_ODOO_VENV_PYTHON=/path/to/venv/bin/python
cargo test -p orchestrator-core
```

Without those set, the two tests still skip — and the summary still says
`ok`. That is worth knowing every time this suite is read as proof.

## Two things real Odoo told us about reloading on save

Building the "restart on save" feature meant asking real Odoo 17 two
questions the source alone answers ambiguously. Both answers changed the
implementation.

### `dev_mode` in `odoo.conf` is read and then thrown away

The obvious implementation writes `dev_mode = reload,qweb,xml` into the
generated `odoo.conf`. That was the first implementation here, and it does
nothing at all.

Odoo lists `dev_mode` in `blacklist_for_save` — commented in its own source
as *"Not exposed in the configuration file"* — and `_parse_config`
recomputes `options['dev_mode']` from the command-line `--dev` option
*after* the config file has been read, overwriting whatever the file said.

Verified against a running Odoo 17 rather than argued from the source. With
`dev_mode = reload,qweb,xml` in the config file and neither `inotify` nor
`watchdog` installed, Odoo logged **nothing at all** — no watcher, and not
even the "autoreload feature is disabled" warning, because the branch that
emits it was never reached. Passing the identical setting as `--dev=` on
the command line immediately produced:

```
WARNING odoo.service.server: 'inotify' module not installed. Code autoreload feature is disabled
```

So auto-reload is a **flag**, never a config line. `Core::start_server`
appends `--dev=reload,qweb,xml` to the argv, and `odoo_conf::render` is
deliberately incapable of emitting the key at all — the parameter was
removed rather than left available to be misused.

### `watchdog` is what makes the flag real

With the flag on the command line and `watchdog` installed into the same
interpreter, the same Odoo logged:

```
INFO odoo.service.server: AutoReload watcher running with watchdog
```

Neither `inotify` nor `watchdog` is in Odoo's `requirements.txt`, so a venv
this app provisions has neither by default. That is why
`Core::set_reload_policy` installs `watchdog` when the setting is turned on:
without it the switch would be a setting that quietly does nothing, which
is the exact failure mode this project keeps finding and removing.

On Linux Odoo prefers `inotify` and names it in the warning, but the
fallback branch (`elif watchdog:`) means `watchdog` satisfies it on every
platform — one dependency instead of a per-platform choice.

## A bug this found: the generated config never asked for host routing

Unrelated to reloading, and worse. The entire address story of this
product — `acme.localhost:8069` opens the `acme` database with no picker in
the way — depends on `dbfilter = ^%d$`. The reverse proxy's module docs,
`Database`'s doc comments, the address decision doc and the UI all describe
that routing.

`odoo_conf::render` never emitted the key. Odoo's default `dbfilter` is the
empty string, which filters nothing.

Measured on real Odoo 17 with two databases and identical configs but for
that one line:

| Request | `dbfilter = ^%d$` | no `dbfilter` |
| --- | --- | --- |
| `GET /web`, `Host: acme.localhost` | `303 → /web/login` | `303 → /web/database/selector` |

Every address on the port landed on the database picker. Fixed, with
`always_asks_odoo_to_route_by_the_first_label_of_the_host` asserting the
line is present.

The reason it survived this long is worth naming: the front end drew its
"the odoo.conf this generates" panel from a **hand-written approximation**
that included `dbfilter`, so the UI showed the config everyone believed in
while the file on disk said something else. That panel now fetches
`GET /servers/:id/odoo-conf`, rendered by the same `odoo_conf::render` the
start path writes, and shows the command line beside it — because after the
`dev_mode` finding, a panel that showed only the file would still have been
hiding half of what runs.


## Neutralization: the thing that could have burned a real client

The most dangerous thing this app makes easy is putting a copy of a
client's production database on a laptop. Until now it did that and nothing
else — the restored copy kept:

- every outgoing mail server, **active, with its SMTP password intact**;
- every scheduled job;
- live payment-provider credentials (56 core modules in Odoo 17 ship a
  `data/neutralize.sql` for exactly this);
- fetchmail servers, still pulling the client's real inboxes.

A local copy left running could email that client's customers and charge
real cards. Odoo solved this in 16 and the app was ignoring the solution.

### Measured, on a real Odoo 17 database

Set up as a production copy would be — an SMTP server named "Client SMTP"
with the password `hunter2` on it, and `base` + `mail` installed:

| | Before | After `odoo-bin neutralize` |
| --- | --- | --- |
| Mail servers with a usable password | 1 (`hunter2`) | 0 — deactivated **and** `smtp_user`/`smtp_pass` nulled |
| Fallback mail server | — | a dummy `neutralization - disable emails` pointing at an invalid host, so a command-line fallback can't take over either |
| Active crons | 12 | 1 (autovacuum only) |
| `ir_config_parameter['database.is_neutralized']` | absent | `true` |

### Two design consequences

**Delegated, not reimplemented.** `Core::neutralize_database` runs
`odoo-bin neutralize`, the same reasoning as `--dev=reload`: the set of
tables to clear depends on the Odoo version *and* on which modules are
installed, so a hand-copied list here would be wrong the first time
somebody installed a module.

**The state is read, not remembered.** That last row above is the useful
one: Odoo records the flag *in the database*. So `neutralization_states`
reads `ir_config_parameter` rather than a column of this app's own — which
means it is right for a database neutralized outside this app, and for one
restored from a dump that was already neutralized before it arrived. There
are four answers, and the fourth matters: `Unknown` (its Postgres isn't
running, so nothing could be read) is deliberately not `Live`. "I couldn't
check" must never render as "it's fine".

Restoring now neutralizes by default — the checkbox is on, and unticking it
replaces the description with what that actually means. The window between
"a client's production data is on this machine" and "it can no longer reach
anybody" should not be a round trip wide.

### Provenance, so the warning isn't noise

A database carries a `DatabaseOrigin` now — created, duplicated, restored
or adopted. The "live data" warning appears only for `restored` and
`adopted`, the two whose contents came from outside this machine. Badging
every empty dev database "not neutralized" would have taught people to
ignore the word.

## A leak found while testing that

A test panicked, and left a postmaster running that blocked every later
run. The cause was worth the detour: `StopOnDrop` shells out to
`pg_ctl stop -D <data_dir>`, and `pg_ctl` finds the postmaster *through*
`postmaster.pid` inside that directory. Take the directory away — a deleted
temp dir, an unmounted volume, a user tidying up while the app runs — and
the polite stop fails and the cluster runs on forever holding its port.

The guard knows the pid. It now falls back to signalling it directly (TERM,
a bounded grace period, then KILL), the same escalation `odoo.rs` uses, and
`a_cluster_whose_data_directory_vanished_is_still_stopped` proves it
against the OS rather than against anything the app believes.

The test suite also picked its Postgres ports by counting up from a fixed
55500, so a cluster one run left behind collided with the next run. It asks
the OS for a free port now, and the suite runs twice back to back.
