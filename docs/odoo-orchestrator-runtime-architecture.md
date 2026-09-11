# Runtime architecture — running Odoo without Docker (researched, Aug 2026)

## The decision
No Docker. Ship a **bundled runtime per Odoo version** + **embedded PostgreSQL**, both as ordinary programs in one app-owned folder that the desktop app starts and stops as child processes. No daemon, no VM, no admin password, no changes to the user's system Python or Postgres. This is the Laravel Herd / DBngin model, which is proven in shipped desktop products.

**Postgres is not a singleton the whole app shares.** An app can register and run any number of independent Postgres instances — each its own self-contained cluster with its own data directory and port, started and stopped independently. This mirrors DBngin itself (see the precedent noted below: "multiple versions/ports side by side"), and matches the same topology-rule reasoning already established for Odoo servers further down this doc: share by default when nothing forces separation (e.g. two projects both fine on the same Postgres major version), but nothing stops registering a separate instance when it genuinely is needed — a different major version under test, or a user who just wants project isolation. See task 2.1 in `odoo-orchestrator-task-breakdown.md` and `orchestrator-core::postgres`/`Core`'s multi-instance registry for the implementation.

Isolation is **process-level, not kernel-level**. The honest cost is production parity — a container matches a Linux production host more closely than a native process on macOS does. For local development that trade is almost certainly right; for a future "deploy to prod" path, containers/OCI images can still be the *output format* without being the local runtime.

## The four building blocks — verification status

### 1. Python runtime — **uv (Astral)** ✅ solid
- `uv python install` downloads and manages standalone CPython builds independent of any system Python.
- Single self-contained Rust binary (~18–20 MB), no prerequisites — vendor the binary directly rather than using the curl/PowerShell installer.
- Native Windows msvc builds for x64/arm64/x86, registers Pythons per PEP 514. Windows is first-class, not an afterthought.
- Builds come from `astral-sh/python-build-standalone` (Astral took the project over from Gregory Szorc). Deliberately GPL-free (libedit not readline, `_gdbm` disabled) so the archives are **redistributable**; each ships license texts for compliance.
- **Relocatability caveat:** the distributions embed build-time absolute paths in `_sysconfigdata_*.py`. uv patches these automatically; raw use of the tarballs does not. So drive everything through uv.
- Linux builds need glibc ≥ 2.17; Windows builds need Win10+ for CPython 3.14+.

**Two Odoo-specific landmines found in Odoo's own `requirements.txt`:**
- **`psycopg2` (what Odoo pins) has PyPI wheels for Windows only** — nothing for macOS or Linux, so it needs a compiler + libpq + `pg_config` there. **Substitute `psycopg2-binary`**, which has full manylinux/musllinux/macos (x86_64 + arm64)/win wheels.
- **`python-ldap` has no wheels at all** (sdist only, needs OpenLDAP headers). Odoo already excludes it on Windows; for a no-compiler install, drop it on macOS/Linux too — it only powers the LDAP auth module.
- Also note: `gevent`/`greenlet` are already excluded on Windows in Odoo's requirements, meaning **Odoo on Windows runs without the longpolling/bus worker**. Know this before promising feature parity across OSes.

### 2. PostgreSQL — **embedded binaries + `initdb`/`pg_ctl`, any number of independent instances** ✅ solid
- Running a cluster from a self-contained directory with no service registration and no admin rights is the normal supported path on all three OSes. On Windows, `pg_ctl register` is optional and simply not used — Postgres runs as a plain child process. (Postgres refuses to run as root anyway, so unprivileged is forced.)
- **Not a singleton:** the app supervises a *registry* of Postgres instances, not one shared cluster — see "The decision" above. Each instance is fully independent (own data dir, own port, own lifecycle), so nothing stops a user or a workstream from running Postgres 15 and Postgres 16 side by side, or keeping two projects' data on wholly separate clusters.
- **Binary sources** (all PostgreSQL License, permissive/redistributable):
  - **zonkyio/embedded-postgres-binaries** — broadest matrix: Darwin/Windows/Linux/Alpine across amd64, i386, arm32, **arm64v8**, ppc64le; PG 11→18.4; ~10 MB trimmed bundles. **Best default.**
  - **EDB "PostgreSQL Binaries"** — trap: current versions are **macOS + Windows x86-64 only**; Linux binaries only exist for the unsupported 9.1–13 line, and there's no Apple Silicon native.
  - **theseus-rs/postgresql-binaries** — Rust-target-triple releases, up to 18.4.
  - **npm `embedded-postgres`** — repackages zonky per-platform; if the app is Electron/Node this is nearly off-the-shelf.
- **Precedent:** DBngin (macOS + Windows) states outright "runs the database server natively. No Docker, no Virtual Machine," with **multiple versions/ports side by side** — the direct precedent for this project's own multi-instance decision above, not just for the no-Docker approach. By contrast Supabase CLI and Neon Local both *require* Docker — a differentiator worth noting.
- Practical: bundle `vcruntime140.dll` on Windows; sign/notarize the macOS binaries inside the app bundle or Gatekeeper quarantines them.

### 3. PDF rendering — **wkhtmltopdf** ⚠️ the weak link, plan around it
- Odoo requires the **patched-Qt build**, not the distro package — the distro version can't render headers/footers, which breaks Odoo report layouts. Version map: Odoo ≤9 → 0.12.1; 10–15 → 0.12.5-1; **16+ → 0.12.6.1-3**.
- **The project is archived.** `wkhtmltopdf/wkhtmltopdf` archived 2 Jan 2023; `wkhtmltopdf/packaging` archived 28 Aug 2023. Last source release 0.12.6 (Jun 2020). No security maintenance, no Qt updates.
- **The platform matrix is lopsided:** 0.12.6.1-3 (May 2023) is **Linux only**. The last builds with macOS and Windows are 0.12.6-1 (June 2020) — macOS **x86_64 only**, Windows via an MSVC installer plus a portable `mxe-cross-win64.7z`.
- **No official Apple Silicon build exists** and Homebrew's cask is disabled — macOS arm64 means shipping the x86_64 binary under **Rosetta 2**.
- **Replacement:** Odoo is building `odoo/paper-muncher`, a from-scratch C++ HTML→PDF engine, with a stated plan to ship both and phase wkhtmltopdf out. **Still proof-of-concept** — no production release, no version ships it by default. A headless-Chromium swap (PR #32624) was never merged.
- **Product response:** ship a known-good pinned build, surface it honestly in the Engine screen (it is not a secret and hiding it would be the black-box behavior senior devs punish), and track paper-muncher as the exit. Odoo mirrors builds at `download.odoo.com/deb/` and `/extra/`.

### 4. Local domain routing — **`*.localhost`** ✅ for browsers, ❌ at the OS level
- **RFC 6761 §6.3 says SHOULD, not MUST** — which is exactly why support is inconsistent.
- **Browsers work:** Chrome/Chromium/Edge hardcode `*.localhost` → loopback, bypassing the OS resolver. Firefox since **84** (Nov 2020). Safari never implemented its own shortcut and inherits the macOS resolver — broken until **macOS 26**, working there (WebKit bug 160504 closed Sep 2025).
- **OS resolvers do not:** bare glibc returns nothing for `acme.localhost` (systemd-resolved does synthesize it, so Ubuntu desktops generally work but containers/Alpine don't); Windows forwards the query to DNS; macOS ≤15 fails. **curl special-cases only the exact name `localhost`**, not subdomains.
- **Consequence:** `acme.localhost:8069` typed into a browser hits the server and Odoo's `dbfilter = ^%d$` routes to the right database with **no /etc/hosts edit and no admin prompt** — but the app's own health checks, an Electron main-process fetch (Node uses the OS resolver), or any CLI tool must use `127.0.0.1:<port>` instead. Bonus: `*.localhost` is a secure context in Chrome/Firefox, so HTTPS-gated APIs work over plain HTTP.

## The topology rule (why the product can even have two shapes)
Verified from Odoo source: **`addons_path` is a process-level setting** on the `odoo.tools.config` singleton, parsed once at startup. There is no per-database override.

- **One process = one addons_path = one Odoo version**, shared by every database it serves.
- **Topology A (one server, many databases)** therefore requires all those databases to be on the **same Odoo major version and the same module set**. Cheap, one process, they start together.
- **Topology B (one server per database)** is the *only* way to have different versions, different branches, or different custom-module sets per project.

This is why the app can and should choose for the user: same version + same addons → offer to share; anything else → its own server, with the reason stated in one sentence. **The same rule now explicitly applies to Postgres instances, not just Odoo servers**: default to sharing one instance when nothing forces separation, but register a separate one the moment something genuinely does (a different major version under test, explicit user-requested isolation) — see "The decision" above.

Supporting mechanics:
- `dbfilter` uses `re.match` (start-anchored, not full) — always write `^…$`. `%h` = Host header minus port and leading `www.`; `%d` = first label of that. Both are `re.escape`d, so no regex injection via Host.
- **`dbfilter` wins over `-d`** — it's a hard branch, not an AND. Setting both and expecting intersection is wrong.
- For topology B, the clean config is `db_name = <db>` + `list_db = False` (hides the database manager entirely).
- `admin_passwd` (master password) gates only DB-manager operations — create/duplicate/drop/backup/restore. Completely separate from a database's admin *user* password. Default is `admin`; stored plaintext still works but Odoo hashes it (pbkdf2_sha512, 600k rounds) and **rewrites `odoo.conf`** when changed via the web manager — the app must expect that file to be edited underneath it.
- Duplicating a DB does `CREATE DATABASE … TEMPLATE` **and** `shutil.copytree` of the filestore, after force-disconnecting the source. Dropping via Odoo removes the filestore; dropping via plain `dropdb` **leaves an orphan filestore directory** — worth a cleanup feature.
- Backup format matters: `zip` = `dump.sql` + `manifest.json` + filestore; `dump` = raw pg_dump, **filestore not included**. A DB restored without its filestore fails *silently* — `_read_file` swallows the IOError and returns empty bytes, so you get blank images and broken PDFs with no error. Label this clearly in any backup UI.
- Ports: `longpolling_port` was renamed `gevent_port` in **16.0** (deprecated alias present in 16.0, gone by 18.0). Emit `gevent_port` for 16+. Only relevant when `workers > 0`; for a dev desktop tool `workers = 0` (threaded) is the right default — required for `--dev=pdb` debugging and avoids the second port entirely.
- Dev flags: `--dev=xml,reload,werkzeug` is a better default than `--dev=all` (which includes debugger hooks that drop a GUI-managed process into pdb on stdin). **`--dev=reload` silently does nothing without the `watchdog` package** — bundle it. And `--dev=xml` covers QWeb templates only; other view types still need `-u <module>`, which is the single most common junior-dev confusion and worth handling explicitly in the UI ("Apply changes" button).
- `web.base.url` is **per-database** and rewritten on admin login unless frozen — so moving a project between ports leaves a stale URL in the DB.

## Net risk ranking
1. **wkhtmltopdf** — the only item that could genuinely sink the plan. Archived binary, no Apple Silicon build, no current macOS/Windows build at the version Odoo 16+ wants, replacement still a prototype.
2. **Windows feature gap** — no gevent means no bus/longpolling worker.
3. `psycopg2` → `psycopg2-binary` substitution (easy, but silent breakage if missed).
4. `*.localhost` — fine if scoped to browser traffic only; never use it for the app's own internal calls.
