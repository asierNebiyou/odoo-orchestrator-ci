# Odoo Orchestrator — Brainstorm Findings & Direction (Aug 2026)

## The vision
"Git for Odoo, locally; GitHub for Odoo, in the cloud." A desktop app (Linux/Mac/Windows) that lets developers and agencies spin up, manage, and switch between multiple local Odoo instances/databases with one click — auto-discovering modules from a linked Git repo, handling install/upgrade natively — with an embedded code editor. The same config model extends to a cloud platform and/or self-hosted infra-as-code, so agencies can eventually deploy client instances the same way, without hand-rolling Kubernetes/Terraform per client.

## Strategic decisions made so far
- **Sequencing:** build the local tool first (the "Git" layer). It's the highest, most personally-felt pain, fastest to demo, lowest infra risk, and its config schema becomes the deployment spec for the cloud/self-host layer later — one data model, multiple runtimes (local / self-hosted / managed cloud), not three separate products.
- **Business model fork (decided, deferred):** tools company first, hosting company later. Don't operate a multi-tenant Odoo hosting service on day one (that's an infra-operator business with uptime/security/support liability). Ship the orchestration tool itself — self-hosted-first, open-core-leaning — and add a "we'll run it for you" managed tier once the tool and its infra-as-code output are proven.
- **Build-vs-borrow:** depend on existing infra primitives (Docker, Kubernetes, Postgres, Terraform-equivalents) rather than reinventing them. Differentiate on the Odoo-aware orchestration layer on top, not on infra plumbing.
- **Don't compete with Odoo SA's own language server (OdooLS)** on code intelligence — they're actively investing there (currently rough/unstable, but a moving target). Differentiate on environment orchestration, DB lifecycle, and multi-instance management instead.

## Validated pain (sourced research, Aug 2026)
- A freelancer's blog post, *"How I manage hundreds of local development Odoo projects,"* documents trying VMs → Docker → a hand-rolled pdm/gitman setup — none fully solved the problem. Direct proof this pain is real and unsolved even by motivated individual developers.
- Recurring named failure modes from forums/GitHub: Odoo.sh install failures return "please check install log" with no traceback; addons-path misconfiguration silently breaks instances; asset-bundle regeneration randomly corrupts dev instances; Odoo Studio customizations vs. git version control is fundamentally unreconciled.
- GitHub issue `odoo/odoo#13721` asked Odoo SA for a `requirements.txt`-style "known good" environment definition — shelved as wishlist-only. Odoo's own maintainers have declined to solve environment reproducibility; that's an open door.

## Competitive landscape — the key finding
A cluster of four products launched in the last 1–2 years positioning as **"cheaper/more flexible Odoo.sh alternatives"**: **OEC.sh**, **Cloudpepper**, **DeployMonkey**, **CICDoo**. All do multi-cloud git-push-to-deploy, staging environments, backups. This proves real, current paying demand (reinforced by an SEO-content glut of "best Odoo hosting 2026" articles).

**Critically, none of them touch local development.** No desktop app, no real embedded editor (CICDoo's "embedded code editor" is a thin browser text box), no local multi-instance orchestration. The "GitHub for Odoo" half of the vision has competitors; the **"Git for Odoo" half — the local-first wedge — is completely unclaimed.**

Existing Odoo dev tooling is CLI/Docker/YAML-only, no GUI, ever:
- **Doodba** (Tecnativa) — the real baseline to beat. Docker images + `repos.yaml`/`addons.yaml` convention, already encodes a "private modules > OCA > core" addons-path priority model. Actively maintained, Odoo 11–19.
- **odoo-helper-scripts** — its own author abandoned it and is rewriting from scratch as **Odood**, concluding scripts don't scale. Independent validation of "a real tool is needed, not more scripts."
- **click-odoo-contrib** — sharp CLI primitives with no orchestration layer on top: `click-odoo-update` does content-hash-based incremental module updates (only rebuild what changed); `click-odoo-initdb` does template-cached instant DB creation; `click-odoo-backupdb`/`restoredb --neutralize` for safe prod-copy refreshes.
- **OCA's `oca-ci`** — the most mature "CI for Odoo modules" prior art: dependency-graph-aware install testing, fails builds on log warnings (not just exceptions). Worth wrapping, not reinventing.
- **Cetmix Tower** — an Odoo module that turns Odoo into a DevOps panel (SSH-orchestrated, multi-stack). Different approach (in-Odoo, not local-dev-first); small adoption.
- **Odoo's official VS Code extension (OdooLS)** — active but explicitly unstable per its own maintainers.

## Technical recommendation: embedded editor
Do **not** fork VS Code OSS (the Cursor/Windsurf path). In April 2025 Microsoft shipped a runtime check that deliberately broke the C/C++ extension in forks (also affects Pylance) — an active, ongoing risk for anyone forking, not a hypothetical. That fight is worth having only if the editor *is* the product; here it's a feature of an ERP tool.

**Better fit:** Eclipse Theia (MIT, purpose-built for embedding/white-labeling, native Open VSX support) or the openvscode-server approach Gitpod built (rides Microsoft's own "VS Code for the Web" build with minimal patching instead of forking ~1.3M lines). Monaco-editor-alone is insufficient — GitLab tried Monaco-only for their Web IDE and outgrew it into a full VS Code implementation.

## Odoo technical mechanics worth knowing (for the module/DB automation layer)
- Module dependency resolution is a flat, path-agnostic topological sort over `depends` in `__manifest__.py`. Odoo has **no concept of module provenance** (client-private vs. OCA vs. core) — that only exists in the addons-path ordering. If two addons-path entries contain a module with the same technical name, Odoo silently uses whichever is found first — a real, currently invisible bug class (**addons-path shadowing**) a tool could detect and surface.
- `click-odoo-update`'s content-hash-based incremental update and `click-odoo-initdb`'s template-cached DB creation are the two most reusable/adoptable primitives found — wrap and GUI-ify rather than reimplement.
- **OpenUpgrade** (OCA) is the only community path for major-version migrations (Odoo Enterprise's paid hosted upgrade is the only official path). It requires manually chaining `OpenUpgrade N` tools version-by-version (no skip-version path) and has **no orchestration wrapper** for backup/checkpoint/rollback across a multi-hop migration chain — a concrete, well-scoped feature opportunity.
- Odoo's own `runbot` builds a live, clickable running instance per PR/branch (not just pass/fail CI) — a strong UX pattern worth adopting.

## Candidate differentiating v1 features (unclaimed by any competitor found)
1. **Branch-based ephemeral DB+filestore snapshots** (Supabase-style branching) — nobody in the Odoo space, including all four hosting competitors, has this. Directly answers the "database errors out, I want to change/delete it" pain from the original brainstorm. Strong demo candidate.
2. **GUI over `click-odoo-contrib` primitives** — one-click instant DB creation (template cache), incremental hash-based module updates, safe prod-copy refresh with neutralization — sharp existing primitives, zero UI today.
3. **Manifest-driven dependency + provenance graph with shadowing detection** — surfaces addons-path collisions Odoo itself can't see.
4. **Doodba-equivalent addons-path priority model (private > OCA > core), but auto-generated from repo scanning** instead of hand-written YAML.
5. **OpenUpgrade chain orchestration** with checkpoints/rollback across multi-version migrations.
6. **Embedded Theia/openvscode-server editor** with Odoo-aware context (instance/DB state visible alongside code), not a bolted-on browser text box.

## Open questions / not yet resolved
- Local pain vs. infra pain: which specific pain costs the user more time *this week* — context-switching between client setups, or fear of breaking something mid-session (upgrade failures)? Determines which killer feature (workspace/context-switching vs. DB snapshot/rollback) gets built and polished first.
- Business/pricing model, open-core boundary (what's free/self-hosted vs. paid), and GTM (agency-first vs. solo-dev-first) — not yet explored, flagged as next brainstorm thread.
- Architecture decision between Theia vs. openvscode-server-in-webview not yet made (both viable, tradeoffs noted above).
