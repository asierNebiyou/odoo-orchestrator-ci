# Design mockups (exported from the Claude design canvas)

These 7 files are the actual visual mockups referenced from
`../odoo-orchestrator-ui-design-principles.md` ("The design canvas that
implements these: https://claude.ai/code/artifact/f2d8ff76-8d94-42cf-b44c-7cfd3efbc5a9").
They were built in an earlier session using Claude's design-canvas tool — a
visual design surface, not code review — and are exported here as static
HTML so this knowledge doesn't live only behind that URL.

**Nobody on this project wrote application code against these.** The
screens actually implemented in `frontend/src/screens/*.tsx` (Topology,
Engine, Modules, Activity) are functional passes proving the frontend talks
to the real Rust backend — they are plainer than these mockups and do not
match them pixel-for-pixel. These files are the design target, not a
description of what's built. Closing that gap is real, currently-open
frontend work.

## Viewing them

Each `.dc.html` file is a self-contained static page — inline CSS, no
build step, no external runtime required to *view* it (there's a small
interactivity layer, "support.js" from the design-canvas tool itself, that
these files reference but that isn't needed for the static layout/visuals
to render — verified by opening them without it). Serve this folder and
open any file, or just open `index.html` for a picker with the design
intent for each screen:

```
cd docs/design-mockups
python3 -m http.server 8080
# then open http://localhost:8080/index.html
```

Opening a `.dc.html` file directly via `file://` mostly works in Chrome-
family browsers too, but a couple of pages fetch a Google Font stylesheet
that some browsers block over `file://` — serving the folder avoids that.

## What each screen is, and why (from the canvas's own annotations)

1. **`Main.dc.html`** — First run. The whole primary job: name it, pick a
   version, go. No repo, no Git, no config file — those come later and
   only when asked for.
2. **`Progress.dc.html`** — Setting up. Real named stages, and the address
   and password shown before it's ready. Ends by telling you the next one
   takes 9 seconds, not 40.
3. **`Topology.dc.html`** — Odoo & databases, the centre of the app. Both
   topology shapes visible at once: one server holding several databases,
   and a database with a server to itself. Odoo's `addons_path` is
   per-server, so the rule is real, not cosmetic (see
   `../odoo-orchestrator-runtime-architecture.md`'s "topology rule").
4. **`WorkspaceDetail.dc.html`** — A database. Address, login, password,
   size, last backup — everything a junior needs on one screen; modules,
   code and logs are tabs, not the front door.
5. **`NewWorkspace.dc.html`** — New database. The shared-vs-own-server
   decision is made for you and explained in one sentence, with an escape
   hatch. Nobody should have to learn `addons_path` to make a second
   database.
6. **`Engine.dc.html`** — No Docker: bundled Python runtime per Odoo
   version, a private Postgres, a pinned PDF binary. One folder, no
   daemon, no admin password — and an honest note about the one fragile
   piece (wkhtmltopdf, per the runtime-architecture doc's risk ranking).
7. **`Editor.dc.html`** — The embedded editor. The rail answers the two
   questions you actually have while coding: which database am I hitting,
   and how do I make this change take effect.

`canvas.json` is the original canvas layout (artboard positions and the
full annotation text above) exported alongside the screens, kept as-is for
provenance.

## How to use this as an implementation reference

Per the design-canvas tool's own convention (echoed in its export
tooling): treat these as a **reference mockup, not production code**. The
markup and inline styles carry the design's precise values — colors, font
sizes, spacing, radii, shadows, layout — which a real implementation
should replicate faithfully in its own components and styling system
rather than copy wholesale. The CSS custom properties block at the top of
each file (`--canvas`, `--ink`, `--acc`, `--ok`, `--warn`, `--err`, etc.)
is the actual design-token palette this project's UI should be built
against.
