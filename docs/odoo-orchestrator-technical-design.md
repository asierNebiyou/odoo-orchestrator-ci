# Odoo Orchestrator — Technical Design: Shell, Plugins, Events, Sandboxing (Aug 2026)

This resolves the two decisions `odoo-orchestrator-project-plan.md` flagged as open in Phase 0 (desktop shell, editor architecture), and answers the newer requirement that wasn't covered by any existing doc: the app needs a plugin-type UI, an event-based core, sandboxing where it matters, and a tech stack chosen with those in mind — not bolted on after the fact. Nothing here contradicts the no-Docker runtime decision in `odoo-orchestrator-runtime-architecture.md`; it's the layer that sits on top of it.

## The shape of the thing

Before picking libraries, the load-bearing idea: **the app is a process supervisor with a UI, and everything it supervises is "a local process on a port."** Postgres, each Odoo runtime, the code editor, and later any hosted-deploy agent are all the same kind of object to the core — something it starts, watches, stops, and emits events about. The UI, the plugin system, and the sandboxing model all fall out of that one idea, which is also why it was worth writing down before choosing a framework.

Concretely, the app splits into three layers:

1. **The core** — a single long-running Rust process. Owns all state (SQLite), supervises every child process (Postgres, Odoo runtimes, the editor sidecar), runs the event bus, and exposes one local HTTP+WebSocket API on `127.0.0.1` with a random port and a per-launch token. Nothing else in the app touches Postgres, the filesystem, or a child process directly — it all goes through this API.
2. **The shell UI** — the main window (Topology, Engine, workspace detail, onboarding, etc. from the existing design). Talks to the core only through that same local API, plus a native event stream for live updates.
3. **Sidecars and plugins** — the code editor (openvscode-server, below) and any future plugin surface. Each is either a separate process the core supervises, or UI rendered in an isolated webview that talks to the core through the same API with a scoped token.

The reason to route *everything* — including the shell UI itself — through one local API instead of giving the UI direct Rust bindings is what makes the rest of this doc coherent: it means a plugin, the editor, a future CLI, and a future hosted-agent all speak the same protocol, and it's the seam where the local tool eventually talks to a cloud control plane without a rewrite.

## Desktop shell: Tauri, not Electron

**Decision: Tauri.** The project plan listed this as a Phase 0 spike; here's the reasoning that settles it rather than requiring the full "build the same screen twice" comparison originally planned.

Electron's case is real — VS Code, Slack, and Docker Desktop all ship on it, Chromium is identical across OSes so there's no webview fragmentation to test for, and if the editor choice had been "embed Theia's own Electron app," Electron would basically be forced. But three things point the other way for this specific app:

- The core's actual job is spawning and supervising OS processes (Postgres, per-version Odoo runtimes, `uv`, the editor sidecar) — that's Rust's home turf, and Tauri's "sidecar" pattern (bundle a binary, spawn it, own its lifecycle, get its stdout/stderr as a stream) is built for exactly this, whereas in Electron the same job means shelling out from Node and hand-rolling the supervision Tauri gives for free.
- Tauri ships without a bundled Chromium — it uses the OS's own webview (WebView2 on Windows, WKWebView on Mac, WebKitGTK on Linux) — so the app installs at tens of MB instead of Electron's 100+ MB baseline and doesn't run a second full browser engine alongside every Odoo instance it's managing. For a tool whose whole pitch is "no Docker, no heavyweight infra," a lighter shell is on-brand, not just an implementation detail.
- Tauri's permission/capability system — each webview declares which core commands and which filesystem/network scopes it may use, enforced by the Rust side — is exactly the primitive the sandboxing requirement below needs. Electron has the pieces (contextIsolation, preload bridges) but you assemble them yourself; Tauri ships the policy layer.

The real cost is webview fragmentation (three rendering engines to test instead of one Chromium), and that risk is manageable because the shell UI is a fairly standard app (lists, forms, panels) rather than something that leans on bleeding-edge CSS/canvas features.

## Editor: openvscode-server as a supervised sidecar, not Theia

**Decision: openvscode-server**, run as a child process the core supervises like any other — bound to `127.0.0.1`, a random port, a per-session token — with the shell UI opening it in a dedicated webview/tab pointed at that local URL.

Both options were already narrowed correctly in the earlier research (avoid forking VS Code OSS; Theia and openvscode-server are the two sane paths). What settles it between those two is the same "everything is a supervised local process" shape from above: openvscode-server is a plain server process you start and stop, which is a perfect fit for a Tauri-sidecar architecture and needs no Electron/Node runtime bundled in just to host it. Theia's production packaging story (Theia Blueprint) is an Electron application in its own right — adopting it would mean either running two desktop-app frameworks side by side, or rebuilding Theia's Electron shell inside Tauri, neither of which is a small task. openvscode-server also carries less ongoing maintenance surface: it's Gitpod's low-patch fork of Microsoft's own "VS Code for the Web" build, versus Theia being a separate IDE codebase to track.

What this buys, concretely: the full Open VSX extension ecosystem for free, a VS Code-identical editing experience (no retraining junior devs on a different editor), and — because it's "just a server on a port" — the same event/API bridge pattern as everything else. A thin first-party extension inside the editor process talks to the core's local API to know which workspace/database/addons-path it's currently attached to, so the editor always knows its Odoo context without the outer app having to inject that state by other means.

## The core API and event bus

The core exposes two things over its local API: request/response commands (`createDatabase`, `startServer`, `duplicateDatabase`, ...) and a stream of domain events (`server.started`, `db.created`, `db.backup.completed`, `module.upgrade.requested`, `editor.attached`, ...). Every state change in the app happens by the core appending an event, updating its own SQLite tables from that event, and then broadcasting it — the shell UI, the editor's context extension, and any plugin all just subscribe to the slice of the event stream they care about instead of polling. Tauri's native event system carries this from the Rust core to the main webview; the same events go out over the local WebSocket API for the editor sidecar and any other subscriber.

Two reasons this is worth the (small) extra structure over a simpler "UI calls a function, function returns a result" model:

- **It's the actual mechanism for the plugin system below.** A plugin declares which event types it wants and gets pushed exactly those, rather than needing bespoke integration points wired in by hand for each new plugin.
- **It's a head start on the cloud phase.** Persisting the event log (a plain SQLite table, not a big-A event-sourcing framework) means the local app already has an audit trail — useful on its own for an "Activity" view — and when the hosting/deploy layer gets built later, syncing "what happened to this workspace" to a control plane is a matter of shipping the same event log upstream, not inventing a new sync protocol. This is a direct payoff of the earlier "one data model, multiple runtimes" decision.

This does not mean full event-sourcing rigor (no event replay to reconstruct state from scratch in v1) — SQLite tables remain the source of truth for current state, the event log is an append-only side effect of writing to them. Keeping it that lightweight avoids over-building infrastructure the v1 product doesn't need yet.

(Update, task 2.3: `create_database`/`duplicate_database`/`drop_database`/`backup_database`/`restore_database` are now real commands against `Core`, each backed by genuine Postgres admin operations in `orchestrator-core::pg_admin` — not simulated. One deliberate scope note: `backup_database` currently writes a plain-SQL `pg_dump` (the "dump" format `odoo-orchestrator-runtime-architecture.md` already flagged as filestore-less), not Odoo's own "zip" format (`dump.sql` + `manifest.json` + filestore). That's the right scope today — there's no real Odoo filestore to back up yet, since 2.2's Odoo process wiring is still pending a real checkout — but whoever wires in real Odoo servers (2.4) should extend the backup format to cover the filestore at the same time, per that doc's explicit warning that a filestore-less restore fails silently rather than loudly.)

## Plugin architecture

Two plugin surfaces, because they're genuinely different problems:

**UI contribution plugins** extend the shell — a future "Migrations" screen, a future "Git" panel, a future "Deploy to hosted" screen are all exactly this kind of plugin, and treating today's built-in screens (Topology, Engine, workspace detail) as "first-party plugins" against the same interface is what makes the app extendable without a rewrite later. A plugin ships a manifest declaring: what it contributes (a nav-rail entry, a panel, a command, a settings section), what events it subscribes to, and what core API scopes it needs (e.g. "read-only access to workspace metadata" vs. "may create/delete databases"). Its UI renders in an isolated webview or iframe, talking to the core only through a scoped bridge that enforces the manifest's declared permissions — a plugin that only asked for read access to workspace metadata cannot call `dropDatabase`, full stop, regardless of what its JS tries to do.

**Core capability plugins** extend what the core itself can supervise — a second database engine, a different Python runtime manager, eventually a hosted-deploy backend. These are lower-level and don't need a UI at all; they register new command handlers and emit their own events into the same bus.

For v1, ship both interfaces but implement every actual screen and capability as an in-repo "first-party plugin" against them, rather than hand-wiring the UI directly to the core. Defer true third-party dynamic loading — someone else's plugin, downloaded and run on a user's machine — to a later phase. That's a deliberate sequencing call, not a gap: the sandboxing story for *trusted, first-party* plugins (webview isolation + capability scopes, both already native to Tauri) is enough to prove the architecture and ship v1; the harder problem of sandboxing *untrusted* third-party code is worth solving once there's an actual plugin ecosystem asking for it, not before. The seam is already in the right place when that day comes — it's additive (a WASM loader alongside the in-repo loader), not a rewrite.

(Update, task X.1: v1's manifest shape and permission model are now real, implemented code, not just this doc's description — see `odoo-orchestrator-task-breakdown.md`'s X.1 entry and `frontend/src/lib/scopes.ts`/`scopedApi.ts`/`plugins.ts` in the repo. Every built-in screen — Topology, Engine, Modules, and Activity — is genuinely a first-party plugin against that interface: each declares its own permission scopes, receives its API access only through a scope-enforced wrapper, and the shell's nav rail/routing is derived from the plugin registry rather than hand-wired. True third-party webview/iframe-isolated loading remains deferred exactly as this doc calls for.)

## Sandboxing — tiered, not all-or-nothing

"Sandboxed where needed" cashes out differently depending on what's being isolated from what, and it's worth being explicit about which tier v1 actually needs:

- **UI plugin isolation (v1, needed now):** each plugin panel renders in its own webview/iframe with no direct access to Node/Rust APIs, only the scoped command bridge described above. This is what stops a buggy or malicious first-party panel from reaching outside its declared permissions. Tauri's capability system gives this natively.
- **Odoo/database process isolation (already exists, not new):** each Odoo runtime runs in its own `uv`-managed Python environment with its own `addons_path`. Postgres is **not** a single private instance the app owns — an app can register and run any number of independent Postgres instances (see `odoo-orchestrator-runtime-architecture.md`'s topology rule, which this now matches explicitly), each its own self-contained cluster with its own data directory and port. This isn't OS-level sandboxing, but real isolation of dependencies and data — per instance, not just per workspace schema.
- **Third-party code sandboxing (explicitly deferred):** running someone else's compiled plugin code safely — via WASM/WASI with capability-scoped imports rather than native dynamic libraries — is the right eventual answer, but it's a v1.5+/v2 concern once third-party plugins are actually a thing, not a v1 requirement to build speculatively.
- **Multi-tenant OS/VM-level isolation (explicitly out of scope for the local tool, in scope for the hosting phase):** this is the "run a stranger's Odoo instance next to another stranger's" problem — Firecracker/gVisor-style microVM isolation — and it only matters once there's a hosted, multi-tenant product. That's the Odoo.sh-competitor phase the sequencing decision already deferred; it doesn't belong in the local desktop tool at all, and pretending otherwise would be over-building for a product that doesn't exist yet.

Being explicit about the tiers is the point: it means "sandboxed where needed" gets satisfied in v1 without inventing infrastructure (a WASM plugin runtime, VM-level isolation) that has no user yet.

## Tech stack summary

- **Core:** Rust. Process supervision, event bus, local HTTP+WebSocket API, SQLite (via `sqlx` or `rusqlite`) for workspace/server/database state and the append-only event log.
- **Desktop shell:** Tauri. Sidecar processes for Postgres (any number of independent instances), each Odoo runtime, `uv`, and openvscode-server; capability/permission scopes for plugin webviews; built-in auto-updater and cross-platform bundler (msi/dmg/AppImage/deb).
- **Shell UI:** TypeScript + React. React over Svelte/Solid specifically because of the "build with AI agents at 10-20x" constraint from earlier in the plan — it's the framework coding agents produce the most reliable, idiomatic output in, given training-data depth, which matters more here than React's slightly heavier runtime given the UI itself is not performance-critical (lists, forms, panels, not a canvas app). Paired with a lightweight query/cache layer (TanStack Query or equivalent) over the core's API rather than a heavy global-state framework.
- **Editor:** openvscode-server, supervised as a sidecar, rendered in a dedicated webview, extended with a thin first-party extension that reads workspace/database context from the core's API.
- **Runtime layer (per `odoo-orchestrator-runtime-architecture.md`, now built — see task 2.1):** `uv` for per-version Python/Odoo runtimes, embedded PostgreSQL (zonkyio binaries) — as any number of independently-supervised instances, not a singleton — and pinned `wkhtmltopdf`.
- **Plugin format:** manifest (contributes + permissions + entry point) + first-party in-repo implementation for v1; WASM/WASI loader as the deferred path for third-party plugins.

## What this resolves in the existing plan

`odoo-orchestrator-project-plan.md`'s Phase 0 listed "editor architecture spike" and "desktop shell choice" as open, to be settled by building throwaway spikes of both options. Both are now decided by architecture-fit reasoning rather than a build-and-compare spike — Tauri and openvscode-server, for the reasons above. Phase 0 should shrink from "spike both, compare" to a single narrower validation: confirm openvscode-server embeds cleanly in a Tauri webview with token-scoped local auth, and confirm Tauri's sidecar lifecycle handles the specific case of a long-running Postgres process cleanly (start, health-check, graceful stop on app quit, recovery from a crash mid-session). That's a days-not-weeks spike, not a framework bake-off. (Update, task 2.1: this validation is done — `orchestrator-core::postgres` implements exactly this lifecycle against a real Postgres process, with real crash detection and recovery, and `Core` now supervises any number of these instances independently; see `odoo-orchestrator-task-breakdown.md`.)

## New risk surface introduced by this design

- **OS webview fragmentation** (WebView2 / WKWebView / WebKitGTK) is a new testing burden Electron would have avoided. Mitigated by the UI itself being deliberately plain (no exotic rendering), but worth a explicit cross-platform UI smoke-test pass before alpha, not just "it worked on the dev's Mac."
- **openvscode-server maintenance drift**: it tracks upstream VS Code releases on Gitpod's schedule, not Microsoft's or this project's — worth a periodic version-pin review, same posture as the already-flagged `wkhtmltopdf` pin.
- **Local API as an attack surface**: binding to loopback and requiring a per-launch token closes the obvious hole (another local process/website hitting the port), but it's worth an explicit test that the token is actually enforced on every command before this ships, not assumed.

## Still open (carried forward, unaffected by this doc)

- How many parallel AI-agent workstreams the user can personally review — the project plan's "3-4 concurrent" is a placeholder assumption, not a confirmed number.
- Business/pricing model and open-core boundary.
- Which specific local pain (context-switching vs. fear-of-breaking-something) gets the sharpest UX polish first.
