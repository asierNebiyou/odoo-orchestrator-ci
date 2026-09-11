# The UI rebuild (Sept 2026) — projects, Odoos, and one screen per thing

This replaced the entire frontend. Read it before changing screens, because
several things that look like free choices are load-bearing.

## Why it was rebuilt

The previous pass had drifted into a pastel consumer-dashboard look — white
cards on a lavender canvas, 18px radii, pill buttons, soft shadows — which
is the exact thing `odoo-orchestrator-ui-design-principles.md` says this
audience sneers at ("would they be embarrassed if a client saw it over their
shoulder"). The design canvas in `docs/design-mockups/` was the agreed
visual language and the code had stopped matching it.

`frontend/src/theme.css` now carries the mockups' own values verbatim —
button padding, card radius, row grids, the tab underline, the six project
swatches. Changing a value there should mean changing the mockups too.

## The shape

    Projects  →  one project's Odoos  →  one Odoo, everything about it

**Projects** are cards. **Everything inside a project is a row.** That split
is deliberate and comes straight from the design doc's Jakob's-law argument:
an instance list has to borrow the `docker ps` mental model, so monospaced
tabular data with a status column is right and cards would signal consumer
product. Projects aren't instances — they're a handful of named places,
closer to a repo list — so a card there earns its place by carrying a
summary that would otherwise cost a click.

Each level is a whole screen, not a pane. The design doc's "design for a
narrow window" constraint (the real viewport is often a ~700-800px column
docked beside an editor) means a master-detail split leaves neither half
usable at the width people actually run this at.

Inside one Odoo, everything is a tab — Databases, Apps, My code, Community,
Config, Logs, Debug, Editor, Browser — with **snapshots in a rail beside
them, never inside a tab**. Reversibility has to be visible *before* an
action, and a safety net you have to go looking for doesn't discharge the
fear it exists for.

## Language

The word **"server" does not appear in the UI**. The thing a person makes is
"an Odoo". Nobody running a client's books has a mental model for a server,
and the domain object really is just an Odoo — `OdooServer` keeps its name
in the code, where the reader is a programmer.

Similarly: "storage" not "Postgres instance" in primary flows, "your module
folders" not "addons_path", "Apply changes" not "-u module_name".

## What's real vs. honestly absent

Real, against the running backend: projects (create/rename/duplicate/delete),
Odoos, databases (create/duplicate/backup/drop on a real cluster), snapshots
(take/restore-with-counter-snapshot/delete), addons sources, the module scan
including shadowing collisions, module install/upgrade/uninstall, the live
event stream, the git branch lookup, start/stop of a real `odoo-bin`.

Deliberately not faked: the **Editor** and **Browser** tabs say what they'll
be and name their blocker (both need the native window, task 0.6). The
**Logs** tab shows the orchestrator's own event log and says plainly that
Odoo's process stdout isn't streamed because no API endpoint exposes it yet.

## The friction ladder, as implemented

`components/Confirm.tsx` renders rung 2/3/4 differently on purpose —
uniform "Are you sure?" dialogs are measurably worse than none, because
visual processing collapses after the *second* exposure to a repeated
warning and trains the autopilot dismissal that carries into the one dialog
that mattered.

- Stop an Odoo — rung 0, no dialog at all.
- Delete a snapshot, delete an empty project — rung 2, plain confirm.
- Restore a snapshot over a live database — rung 3, **counter-snapshot
  pre-checked**. This is the single highest-leverage element in the whole
  design doc.
- Drop a database — rung 4, type the name.

Deleting a project **never cascades**. The core refuses (409) while it still
holds Odoos, and the UI says so up front instead of letting someone confirm
something that then fails. A one-button action that quietly performed
several rung-4 deletions is the exact blast-radius mismatch the ladder
exists to prevent.

## Gamification: what's there and what isn't

The project cards and the snapshot rail show **protection coverage** ("1 of
3") and real counts. That's an instrument: loss aversion pointed at a
genuinely unprotected database, dischargeable in one click, and nobody feels
judged. There are no points, no badges, no streaks, no score — the design
doc rejects each of those by name for this audience, and the rejection is
not stylistic (overjustification damage is worst for populations with
pre-existing high interest, which is exactly a senior dev).

## The accent is violet (OKLCH 292), not the blue the doc proposed

The doc's rule is that the accent must never be mistakable for a status
signal, measured as hue distance from the four fixed semantic hues (err 22,
warn 78, ok 155, run 235). Violet 292 sits 57 degrees from the nearest;
the blue 268 the doc had proposed sits 33. The mockups' original violet
satisfies the doc's own rule better than the correction did.

## Backend change this required

`Project` is a real entity (`orchestrator-core::model::Project`, a `projects`
table, `project_id` on `servers`, four events, and
`/projects` + `/projects/:id` + `/projects/:id/duplicate` on the API), with
four tests in `orchestrator-core`. Odoos written before projects existed are
adopted into a default project on `Core::open`, idempotently — so the top
level is never mysteriously empty on an existing database.

`duplicate_project` copies **setup only** — each Odoo's version, storage and
addons path, on ports nothing else claims. It never copies databases, and
the menu item says so.

## Running it without a desktop

`cargo run -p orchestrator-api --example headless -- <data-dir> <port>`
boots exactly what the shell boots (real Core, real SQLite, real router,
real bearer token) and prints its base URL and token. It exists because the
shell binary is a genuine Tauri app now and can't build where there's no
webview — but nothing about the API or the core needs a window, and the UI
should stay verifiable against the real backend.
