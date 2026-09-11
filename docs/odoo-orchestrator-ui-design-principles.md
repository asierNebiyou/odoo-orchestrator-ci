# Odoo Orchestrator — UI design principles (researched, Aug 2026)

The design canvas that implements these: https://claude.ai/code/artifact/f2d8ff76-8d94-42cf-b44c-7cfd3efbc5a9

## The core reframe
The audience is skeptical senior developers who currently use Docker/CLI and would sneer at a consumer-SaaS dashboard. Two rules fall out of the research and govern everything else:

- **Instrument, not scoreboard.** Every mechanic professionals accept (GitHub's heatmap, the Lighthouse ring, a CI pipeline graph) reads as an *instrument* measuring the work, never a *scoreboard* judging the person. Two tests: would a senior dev screenshot this to win a PR argument (instrument), and would they be embarrassed if a client saw it over their shoulder (cut it).
- **The product is reversibility.** The core emotional problem is fear of breaking a client's database. Reversibility must be visible *before* the action, not offered after.

## Psychology principles applied

**Cognitive load.** Chunk to 5±2 top-level facts per row; everything else one level down (Miller). Progressive disclosure per-object, not via a global basic/advanced toggle — a senior dev leaves the one problematic instance expanded and the rest collapsed. Hick's law applies to destructive menus specifically: `Delete`/`Drop` never sits in the primary action row.

**Jakob's law — borrow the `docker ps` mental model.** The instance list is a **table**, not cards. Cards signal consumer product; monospaced tabular data with a status column signals "built by someone who has run `docker ps`." Keep column semantics identical to their CLI equivalents. Diverging from a CLI convention spends trust.

**Confirmation dialogs stop working after the second exposure — this is measured.** fMRI research (Anderson et al., CHI 2015 / MIS Quarterly 2018) found a dramatic drop in visual-processing activity after only the *second* exposure to a repeated warning. Uniform "Are you sure?" dialogs are actively **worse than none** because they train autopilot dismissal that carries into the one dialog that mattered. Polymorphic warnings (varying appearance/required confirmation string) resist habituation substantially better.

**Friction ladder keyed to blast radius** (rung 0–4):

| Action | Rung | Pattern |
|---|---|---|
| Stop instance | 0 | Immediate, no dialog |
| Uninstall module | 1 | Immediate + undo toast (≥8s) + auto-snapshot |
| Drop dev database | 2 | Confirm dialog, named object, restorable-from-trash note |
| Restore snapshot over live DB | 3 | Confirm + quantified consequence + counter-snapshot offered |
| Delete instance incl. volumes | 4 | Type-to-confirm the exact database name |

**Confirmation copy needs four elements** or it's decorative: named object, quantified consequence, reversibility status, ripple effects. Button label says the action (`Restore pre-migration-2`), never `Confirm`. Escape dismisses safely; the destructive button is never Enter-focused.

**Soft-delete everything and put the trash in the nav.** A visible "Recently dropped (2)" nav item is a permanent ambient reminder that the safety net exists — which is what actually changes behavior.

**Show the underlying command.** The strongest anti-toy signal, from a CHI 2006 IBM study finding trust (not capability) drives sysadmin CLI preference: GUIs are distrusted because they hide what they actually did. Every operation exposes its exact command, copyable. A GUI that shows its work is a front-end; one that doesn't is a black box, and senior devs reject black boxes over client data.

**Performance is a design constraint.** Doherty threshold: <400ms for every UI-local action, which means **never block the UI on Docker** — cache last-known state, render immediately, reconcile in background. Spinners under 1s make an app feel *slower*. Indicator by duration: <1s nothing; 1–3s skeleton matching real geometry; 3–10s determinate progress + step label; >10s percent-done + streamed log (for devs the log tail IS the progress bar, and it's more honest than a bar).

**`starting` is a first-class state**, not a spinner between stopped and running. Optimistic UI that lies about container state destroys trust permanently.

**Status needs 3 signals, not 1** (Carbon): color + shape + text. Running = green filled dot + `running`; stopped = gray hollow dot; starting = blue ring; error = red triangle + **the actual exit code** (`exited (1)`) — that detail does more for perceived competence than any icon set.

**Two-tier errors.** Plain-language headline plus suggested action, with raw stderr in a collapsed, one-click-copyable Details block. Consumer tools hide stack traces; dev tools file them one keystroke away.

**Design for resumption, not just operation.** Parnin's research (10,000 sessions, 86 developers): 10–15 minutes to resume editing after an interruption; ~one uninterrupted two-hour session per day. Consequences: every long operation must be backgroundable (passive waiting is an interruption you're inflicting — and active waiting feels much shorter), the app restores full state on reopen, and a per-instance timestamped activity log ("where you left off") collapses the rebuild into a glance.

**Design for a narrow window.** The real viewport is a ~700–800px column docked beside an editor, not 1440px fullscreen. A layout that only works fullscreen charges a window-switch on every use.

## Gamification: what shipped, what was rejected

**Correction (Aug 2026):** an earlier version of this doc listed a 0–100 instance health-score ring as shipped. It was cut in a later mockup pass — none of the published screens (Topology/WorkspaceDetail/Engine/Modules) render one, and none of the code scaffolding's `ServerState`/event model carries a score. The other instruments below absorbed its job: **protection coverage** and **time-saved arithmetic** give the same "instrument measuring the work" feedback as concrete facts instead of a computed score, which sidesteps the ring's own legitimacy requirement (published weights, thresholds anchored in real distributions) rather than solving it. Treat "shipped" below as still accurate for the remaining six.

**Shipped (all instruments):**
- **Named staged progress** — directly answers Odoo's "please check install log" with no traceback. Decompose into real timed stages; failed stage stays red and expandable at the killing log line.
- **Time-saved as arithmetic** — "hash-matched 78 of 112; a full `-u all` took 11m40s last week." A fact, not a prize. Informational competence feedback is explicitly in the "does not undermine intrinsic motivation" column of the overjustification research.
- **Endowed progress on setup** — opens at "4 of 7" because those four are genuinely already true (Docker detected, Postgres found, image cached, credentials present). Nunes & Dreze: pre-filled stamps produced ~20% faster completion at identical real effort. A real head start, not a faked one.
- **Protection coverage instead of streaks** — "6 of 8 databases protected." Loss aversion pointed at an unprotected client database, which is a tension the user *wants* to feel and can discharge in one click. Nobody feels judged.
- **Activity heatmap without a streak counter** — GitHub kept the heatmap and deliberately removed the streak number in 2016; the ICSE 2021 natural experiment found removal reduced weekend work and trivial commits.
- **Progressive shortcut disclosure** — surface the keyboard shortcut after the user has done it manually a few times. The only mechanic that builds real competence rather than simulating it.

**Rejected, with reasons:**
- No points/XP/currency — overjustification damage is worst for populations with pre-existing high interest, which is exactly this audience.
- No badges for routine actions (GitHub's own achievements are the cautionary tale — a badge for "merged a PR without review").
- No leaderboards ranking humans (Disneyland's "electronic whip"; any agency-visible ranking becomes a performance-management tool within a week).
- No consecutive-day streaks — the exact mechanic GitHub deliberately deleted.
- No confetti/mascots/level-up sounds — senior devs read animation as latency.
- No fake or padded progress — this audience will time it against `--verbose` and never trust you again.
- No unexplained score, no re-engagement notifications, no locking paid features behind progression.
- Meta-analytic ceiling worth knowing: gamification barely moves *competence* (g=0.277). It amplifies existing motivation; it cannot create it. If a workflow is unloved, fix the workflow.

## Visual craft (implementable)

**Type: Geist + Geist Mono** (both on Google Fonts, free) — the current premium-dev-tool signature. Monospace for *every* machine-generated value: ports, DB names, versions, hashes, timestamps, counts, keyboard hints. Proportional type on a container ID reads as a marketing site.

**Scaling negative letter-spacing** — large text tightens, small text doesn't: 48px/-0.02em, 24px/-0.015em, 20px/-0.01em, body 13-14px/-0.008em, caption 12px/0. Variable weights off the round numbers (510/590 rather than 500/600).

**Body text 13–14px, not 16px.** Density signals professional tooling.

**Dark mode is the default, not the only mode.** A later research pass (NN/g's readability review) found light mode measurably better for small-text accuracy with no measurable eye-strain difference, and recommends light as the default for general audiences while still offering dark for extended-reading tools — this audience fits both halves of that, so the shipped screens have a real toggle rather than dark-only. Dark: elevation by lightness, never shadow. Canvas `#08090a`, surfaces `#0f1011` → `#141516` → `#1a1b1d` — increments of ~5 hex points; the restraint is the whole trick. Hairlines `#212327`/`#2e3036`. Never `#000` (visual fatigue, OLED scroll smearing) and never `#fff` text (halation) — ink tops out at `#f7f8f8`, then `#c9ced6`, `#8a8f98`, `#5f636b`. Light mode mirrors the same scale inverted (see `theme.css` in the code scaffold and the `[data-theme="light"]` blocks in the mockups) rather than being a bolted-on afterthought.

**Hairline borders beat shadows**; shadows only for true overlays, as a ring-then-shadow pair (`0 0 0 1px <border>, <soft shadow>`) with negative spread on modals.

**Accent: blue, OKLCH hue 268°** (dark `oklch(0.60 0.20 268)`, light `oklch(0.50 0.20 268)`) — **correction (Aug 2026):** an earlier version of this doc said "violet, nodding to Odoo's brand without copying it" without having actually checked that choice against anything. A later pass ran the check UXPin's brand-vs-semantic-color research calls for: measuring hue-wheel separation against this system's four fixed semantic colors (err 22°, warn 78°, ok 155°, run/starting 235°) so the accent never gets mistaken for a status signal. 268° sits 33° from the nearest semantic hue (run, 235°) — the original 255° pick was only 20° away and too close by the system's own rule. Blue was still the right call on user preference and on ITBee's SaaS-differentiation point (ubiquity is a real cost, but this system's separation and desaturation rules carry the differentiation, not the specific hue-family), it just needed the same rigor as everything else in this doc rather than being assumed. Desaturated 15–25% for dark, authored in OKLCH so equal lightness reads equally light across hues. Used once per screen; saturated color reserved exclusively for state and danger.

**Radii 4/6/10** — nothing premium uses 16px+ on standard controls.

**Motion:** press 80–100ms, micro 100–150ms, standard UI 150–250ms, modal 200–300ms, **300ms hard ceiling**. `ease-out` for user-initiated, never `ease-in`, never `linear` (except progress). `transform`/`opacity` only. Do **not** animate keyboard-initiated actions or anything done 100+ times a day — animating keyboard nav is the fastest way to make a pro tool feel slow. Always ship `prefers-reduced-motion`.

## The three highest-leverage moves
1. **The pre-checked "snapshot before running" box** on every rung-2+ dialog — converts the core fear into a solved, visible fact at the exact moment the fear occurs.
2. **The persistent command log** showing exactly what was executed — the difference between a front-end and a black box.
3. **Never block the UI on Docker; keep local interactions under 400ms** — slowness in a dev tool doesn't read as loading, it reads as unreliable, and unreliable is fatal for a tool touching client data.
