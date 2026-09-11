# Continuing this project on a real desktop

Everything genuinely buildable and verifiable in a cloud sandbox is done —
see `README.md`'s "What's next" section for the full status. What's left
(0.6, 0.7, all of Workstream 4, X.3, X.4, and possibly the rest of 2.2/2.4/2.5)
needs things a cloud sandbox structurally can't provide: a real desktop
windowing session for Tauri, and/or a real Odoo source checkout. This file
is the handoff note for picking that up on your actual machine.

## All the project knowledge is in this repo now

Everything that was previously only in the Claude Project attached to the
cloud session (six research/design/planning docs) is copied verbatim into
`docs/` in this repo, since a local Claude Code session has no access to
that Project:

- `docs/odoo-orchestrator-research-and-direction.md` — the original
  brainstorm: vision, competitive landscape, validated pain, why this is
  worth building.
- `docs/odoo-orchestrator-runtime-architecture.md` — the no-Docker runtime
  decision (uv + embedded Postgres + wkhtmltopdf), with the real Odoo
  mechanics (dbfilter, addons_path, backup formats) this project's code
  depends on getting right.
- `docs/odoo-orchestrator-technical-design.md` — the Tauri/openvscode-server/
  event-bus/plugin-architecture decisions and reasoning; this is the doc
  that defines what 0.6/0.7/Workstream 4/X.1 actually are.
- `docs/odoo-orchestrator-project-plan.md` — phases, workstreams, non-goals.
- `docs/odoo-orchestrator-ui-design-principles.md` — the "instrument, not
  scoreboard" design system this project's UI (including the friction
  ladder and Activity screen) is built against.
- `docs/odoo-orchestrator-task-breakdown.md` — the authoritative, current,
  task-by-task status (same content as the Claude Project's copy at the
  point this was handed off) — this is the one to read first for "what's
  done, what's not, why."

Treat these as read-only reference, same as they were in the cloud session —
if you update project direction locally, update these files directly (there
is no separate Project to sync back to from a local session).

**Also now included: `docs/design-mockups/`** — the 7 actual visual design
screens (`.dc.html` files: Main, Progress, Topology, WorkspaceDetail,
NewWorkspace, Engine, Editor) exported from the Claude design canvas that
`odoo-orchestrator-ui-design-principles.md` only used to reference by URL.
Serve that folder and open `index.html` to browse them. Read
`docs/design-mockups/README.md` first — it's explicit that these are the
*design target*, not a description of what's implemented: the actual
screens in `frontend/src/screens/*.tsx` are functional-but-plainer passes
proving the frontend talks to the real backend, and do not match these
mockups pixel-for-pixel yet. Closing that visual gap is open frontend work
that doesn't need a real desktop or a real Odoo checkout — it's buildable
right now, local or cloud sandbox alike, against the color/spacing/type
tokens these mockups' inline styles define.

## Setup

1. Install Claude Code CLI if you haven't:
   ```
   curl -fsSL https://claude.ai/install.sh | bash
   ```
2. `cd` into this folder and run `claude` to start a session there. That
   session runs natively on your OS — no VM, no throttled network, real
   `sudo` if you need it.
3. Point it at this file, `docs/odoo-orchestrator-task-breakdown.md`, and
   `README.md` (particularly "What's next") for full context — a fresh
   local session has no memory of the cloud session
   that built this.

## Task 0.6/0.7 + Workstream 4 + X.3/X.4 — the real Tauri build

This sandbox couldn't build a real Tauri window at all — Linux needs
`webkit2gtk`, which isn't installed and can't be without root. **On macOS
this constraint mostly doesn't exist**: Tauri uses the system `WKWebView`
framework, which ships with the OS — there's no separate webview package to
install. What you actually need on macOS:

- Xcode Command Line Tools: `xcode-select --install` (if not already
  present — check with `xcode-select -p`).
- A working `cargo`/`rustc` (this machine already appears to have
  `~/.cargo`/`~/.rustup` — verify with `cargo --version`).

Then, per `README.md`'s "Why no Tauri window yet" section, the actual work
is:

1. Uncomment the `tauri` dependency in `src-tauri/Cargo.toml`.
2. Turn `src-tauri/src/main.rs` into a real Tauri `Builder::default()` app
   that starts `orchestrator-core`+`orchestrator-api` in `setup()` and opens
   a window pointed at the frontend dev server (`tauri.conf.json` already
   has the dev URL and CSP scoped to `127.0.0.1:*`).
3. Pass the API's base URL and bearer token into the frontend via a Tauri
   `invoke` call or an injected global at boot, replacing the
   `VITE_API_BASE_URL`/`VITE_API_TOKEN` build-time env vars flagged in
   `frontend/src/lib/api.ts` as dev-only wiring.
4. `cargo tauri dev` (install the CLI first: `cargo install tauri-cli`, or
   use `npm create tauri-app` tooling if preferred) to actually open a
   window and confirm it renders.

Once that works, 0.6 (openvscode-server sidecar in the webview) and 0.7
(Tauri sidecar lifecycle for `PgSupervisor`) become concrete to validate
against a real window, and Workstream 4 (the embedded editor) and X.3/X.4
(packaging, cross-platform webview smoke test) become reachable.

## Task 2.2 (remaining)/2.4/2.5 — a real Odoo checkout

This sandbox's egress blocks `github.com`/`codeload.github.com`, and real
Odoo is only distributed from there. **Check whether you already have a
real Odoo checkout on this machine** — your home directory has folders
named `odoo-dev`, `erp`, `gilando-erp`, and `gilando-erp-1` that weren't
inspected during the session that built this. If one of them is a real
Odoo source tree, you can wire `OdooSupervisor` (already built and tested
standalone in `orchestrator-core::odoo` — see README) directly against it
with **no network access needed at all**. If none of them are, cloning a
real Odoo release (`git clone --branch 17.0 --depth 1
https://github.com/odoo/odoo.git` or similar) from your own machine's
normal network access is the more straightforward path than anything a
sandboxed agent environment could do.

Once a real checkout exists: generate a real `odoo.conf` from a server's
addons-path (already resolvable via `orchestrator-core::modules::scan`) and
assigned `PostgresInstance` (already a required, resolved field on every
`OdooServer`), wire `Core` to actually call `OdooSupervisor`, and confirm
against the real running process — completing 2.2, then 2.4 (module
install/upgrade/uninstall, needs `click-odoo-update` or equivalent against
that real process), then the rest of 2.5 (the Modules screen's deferred
"N up to date" instrument).
