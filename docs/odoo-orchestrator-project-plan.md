# Odoo Orchestrator — Project Plan v1 (AI-agent-augmented build)

## Assumptions
- Solo founder-led (Asi) directing multiple AI coding agents in parallel. Plan assumes roughly 3-4 concurrent workstreams as a realistic human-review ceiling — adjust workstream count up or down based on actual review bandwidth; more parallel workstreams than you can carefully review is a net negative, not a speedup, especially since this code touches client databases directly.
- v1 scope is the local-first desktop tool only. Cloud/self-hosted deploy layer is deliberately out of scope for v1 (per the earlier tools-company-first decision), but the workspace schema from Phase 0 must be designed to extend cleanly into it later — one data model, multiple runtimes.
- Embedded editor is IN v1 scope. (Revised from an earlier "defer to v1.5" recommendation — with AI-agent build throughput, the engineering-cost argument for deferring it weakens, since it can run as a parallel workstream instead of consuming sequential time.)

## Phase 0 — Foundational decisions (days, not weeks)
Blocking work that must land before parallel workstreams start — getting these wrong means rework across every downstream workstream.
- **Desktop shell and editor architecture: decided, not just spiked.** `odoo-orchestrator-technical-design.md` resolves both by architecture-fit reasoning: **Tauri** for the shell (Rust core built for supervising the Postgres/Odoo/uv sidecar processes this app already needs, native capability/permission scopes for plugin sandboxing, no bundled Chromium) and **openvscode-server run as a supervised sidecar process** for the editor (fits the "everything is a supervised local process" shape better than Theia's own Electron packaging, lower ongoing maintenance surface). What's left is a narrow validation spike, not a bake-off: confirm openvscode-server embeds cleanly in a Tauri webview with token-scoped local auth, and confirm Tauri's sidecar lifecycle handles a long-running Postgres process cleanly (start, health-check, graceful stop on quit, recovery from a mid-session crash). Days, not weeks. **Still blocked** — both spikes need a real desktop environment (Linux `webkit2gtk` / Windows WebView2 / macOS WKWebView) and could not be run in the sandbox this project's scaffolding was built in; see `odoo-orchestrator-task-breakdown.md` items 0.6/0.7.
- **Lock the "workspace" schema v1:** the config object describing {repo(s)+refs, Odoo version, addons sources with priority order, DB(s), env/secrets}. Every workstream in Phase 1 builds against this; it later becomes the infra-as-code spec for the cloud/self-host layer. Should also match the event/API shape from `odoo-orchestrator-technical-design.md` (core owns this as SQLite state, mutated only via the local API) so Phase 1 workstreams aren't reaching into shared state directly. **Started, not locked:** a first-cut schema (`OdooServer`, `Database`, `AddonsSource`/`SourceKind`) exists in code (`orchestrator-core`) and is exercised by passing tests — including real manifest-scanning tests now that workstream 1 is done — but it still doesn't have git refs, env/secrets, or snapshot metadata, so treat it as a working draft workstreams can build against, not the final locked v1 shape.
- **Scaffolding started, and Workstream 1 completed, across two sessions.** There is now a real (compiling, tested) codebase implementing the shape `odoo-orchestrator-technical-design.md` describes: a framework-agnostic `orchestrator-core` Rust crate (domain model + SQLite store + event bus + manifest parser + shadowing/dependency-graph engine, 23 passing unit tests), an `orchestrator-api` crate exposing it over a local loopback-bound, bearer-token-gated HTTP+WebSocket API (axum), a headless `orchestrator-shell` binary standing in for the eventual Tauri window, and a Vite+React+TS frontend with two screens genuinely wired end-to-end: Odoo & databases (Topology) and Modules. Both were verified with actual Playwright browser sessions, not just `cargo test`/`npm run build` — Topology's WebSocket live-push path, and Modules' addons-path shadowing detection against a real on-disk collision. Full details, what's real vs. stubbed, and how to run it: see `README.md` in the `odoo-orchestrator` repo scaffold. Granular task-by-task status lives in `odoo-orchestrator-task-breakdown.md`.

## Phase 1 — Parallel build workstreams (target: 1-3 weeks depending on workstream count)
Four workstreams independent enough to run concurrently:
1. **Manifest & dependency graph engine — done end-to-end.** Parses `__manifest__.py` across the addons-path (a hand-rolled parser for the Python dict-literal subset manifests use, no Python runtime dependency), builds the dependency graph with source provenance (private/OCA/core) and a topological install order, and detects/surfaces addons-path shadowing collisions — verified against a real on-disk collision through both the API and an actual browser session on the Modules screen, not just unit tests. See `odoo-orchestrator-task-breakdown.md` workstream 1 and the scaffolded repo's README for how it was checked.
2. **DB/module lifecycle GUI** — wrap `click-odoo-contrib` primitives behind one-click actions: `click-odoo-initdb` template-cached instant DB creation, `click-odoo-update` hash-based incremental module updates, `click-odoo-backupdb`/`restoredb --neutralize` for safe prod-copy refreshes. *Only bare create/list of databases exists so far (no process invocation yet — see workstream 2 in the task breakdown). This is now the natural next workstream once a real desktop environment is available for its 0.7 spike.*
3. **Branch-based snapshotting** — Postgres template-DB + filestore snapshot (hardlink/rsync) tied to git branches; instant clone, named snapshots, one-click revert. This is the headline demo feature — nobody in the Odoo space has it. *Not started; blocked on workstream 2's Postgres supervisor.*
4. **Desktop shell + embedded editor integration** — wire the Tauri shell and the openvscode-server sidecar (per `odoo-orchestrator-technical-design.md`) into the app, bound to the workspace schema so the editor always knows which instance/DB/branch context it's in via the core's event/API bridge. *The API and event bridge this depends on now exist and are tested (including the new addons-sources/modules endpoints); the actual Tauri window and sidecar wiring are blocked on a real desktop environment (see Phase 0 above).*

Each workstream should produce something independently clickable/demoable against a throwaway test Odoo project before integration starts — "done" means you can see it work, not just that code was reviewed.

## Phase 2 — Integration (the real bottleneck — protect calendar time here)
One workspace, one UI: workspace creation → module discovery/install → branch/snapshot controls → embedded editor, all operating on the same live workspace state. Expect this to compress less under agent leverage than Phase 1 — it's fundamentally a human review-and-reconcile loop between four independently-built pieces, not a write-more-code problem.

## Phase 3 — Dogfood on real client instances
No agent multiplier applies here by definition. Run the tool against your actual client repos/DBs. Track every papercut and break as a concrete logged bug, not a vague impression — this phase is the acceptance test for "no hibi jibbies, 100% satisfactory."

## Phase 4 — Narrow external alpha
A handful of other agency devs, once dogfooding stops surfacing *new categories* of bugs (not zero bugs — new categories).

## Explicit non-goals for v1
- No cloud/managed hosting.
- No infra-as-code export yet (the schema should anticipate this, not implement it).
- No CI/CD pipeline automation yet (OCA's `oca-ci` conventions are the reference for later).
- No OpenUpgrade chain orchestration yet (flagged as a strong v1.5 candidate).
- No third-party/dynamically-loaded plugins yet (per `odoo-orchestrator-technical-design.md`, v1 ships the plugin *interface* but only first-party in-repo plugins against it; true third-party plugin loading and its WASM sandboxing are deferred).

## Key open risk
Review/integration bandwidth is the actual constraint on velocity now, not code-writing throughput. Size Phase 1's workstream count to what can be carefully reviewed — a fast bug in agent-written orchestration code that touches a client's database costs trust, not just time.

## Still open (from brainstorm, not yet resolved)
- Business/pricing model, open-core boundary, GTM (agency-first vs. solo-dev-first) — not yet explored.
- Which specific local pain (context-switching vs. fear-of-breaking-something) gets the sharpest UX polish first — Phase 1's four workstreams cover both, but UI priority within Phase 2 should still be decided deliberately, not by default.
- How many parallel AI-agent workstreams to actually run given real review bandwidth — flagged above as an assumption, not yet tested against how review went for this session's scaffolding work.
