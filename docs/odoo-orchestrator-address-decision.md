# Local addresses: why `.localhost` and not `.odoo` (Sept 2026)

**Decision: databases answer on `<name>.localhost` by default, through this
app's own proxy. No privileged setup, no system state, works on day one.**
Custom domains stay available per database for anyone who wants something
else.

This was researched rather than assumed, because getting it wrong is
expensive in a way that only shows up months later.

## What was rejected, and why

### `.odoo` — actively risky, not merely unreserved

`.odoo` is not a reserved TLD. Anything can happen to it, and something
plausibly will: **ICANN opened its new-gTLD application window on 30 April
2026** — the first new round in over a decade, explicitly including
`.brand` TLDs. A company called Odoo applying for `.odoo` is not a
far-fetched scenario; it's the obvious one.

The precedent is exact and recent. Developers used `.dev` for local work
for years. Google acquired the TLD, added the entire thing to Chrome's
**HSTS preload list**, and in Chrome 63 every `myapp.dev` on every
developer's machine started force-redirecting to HTTPS. Local setups broke
overnight, and nothing the developers had done was wrong — the ground
moved under them.

Shipping `.odoo` would be volunteering for that. It's a *time bomb in
someone else's hands*, which is precisely the sort of thing a tool touching
client databases should not install on a machine.

### The privileged setup `.odoo` would have needed

Bare `acme.odoo` (no port) requires two things, both privileged and both
persistent:

1. **Name resolution.** `/etc/resolver/odoo` plus something answering DNS
   on `127.0.0.1:53` — a privileged daemon. Laravel Valet does this with
   `brew install dnsmasq`.
2. **Port 80.** A root launchd job, or a `pfctl` redirect. Another
   elevation, another persistent system change.

That means an uninstall obligation covering a resolver file, a launchd
plist and a pf anchor. Miss one and a machine keeps intercepting DNS for a
tool that's been deleted. It's also macOS-only mechanics — Windows has no
`/etc/resolver`, so that platform would need per-domain hosts entries and
its own elevation each time.

For an audience the UI design principles describe as distrusting tools that
do things they can't see, a sudo prompt installing a DNS interceptor to
save typing `:8080` is a bad trade.

## What was chosen

`.localhost` is reserved by **RFC 6761** and cannot be delegated to anyone,
ever. Modern browsers resolve `*.localhost` straight to loopback with **no
configuration at all** — Microsoft's own current ASP.NET Core documentation
describes exactly this behaviour and ships a `--localhost-tld` template
around it.

So `acme.localhost:8080` works the moment the app starts, on every
platform, with nothing installed and nothing elevated.

### The honest caveats

- **Safari on macOS does not resolve `*.localhost` subdomains.** Microsoft's
  docs state this outright and tell you to use plain `localhost` there.
  Chrome, Edge and Firefox (84+) are fine. This is a real limitation, not a
  rounding error, and the UI should say so rather than letting someone
  conclude the app is broken.
- **Non-browser clients** (curl, health checks, anything using the OS
  resolver) treat `*.localhost` as an ordinary name and may fail to
  resolve it. Nothing internal to this app should ever address a database
  by hostname — always `127.0.0.1:<port>`. The runtime-architecture doc
  already says this; it remains true.

## If bare hostnames are ever wanted

Use **`.test`**, never `.odoo`. `.test` is also reserved by RFC 6761 and
can never be delegated, so a resolver entry installed for it is safe
forever. That is the TLD Laravel Valet moved to after `.dev` burned it,
and for the same reason.

The shape that would take: an explicit, opt-in "use pretty addresses"
action that states exactly what it will change on the machine, and can undo
all of it. Not something done during install, and never something done
silently.

## Where this lives in the code

- `Database::hostname()` (`orchestrator-core/src/model.rs`) — the default
  suffix, with this reasoning inline so it doesn't get "tidied" back.
- `orchestrator-api/src/proxy.rs` — rewrites `Host` to
  `<database>.localhost` before forwarding. Odoo's `dbfilter = ^%d$` only
  reads the *first label*, so the suffix is irrelevant to Odoo itself —
  which is what made this a one-line change rather than a migration.
- `frontend/src/lib/format.ts` — `databaseUrl()`, and the proxy port.

## Sources

- ICANN, "ICANN Opens Application Window for New Generic Top-Level Domains"
  (30 April 2026) — https://www.icann.org/en/announcements/details/icann-opens-application-window-for-new-generic-top-level-domains-30-04-2026-en
- Mattias Geniar, "Chrome & Firefox now force .dev domains to HTTPS via
  preloaded HSTS" — https://ma.ttias.be/chrome-force-dev-domains-https-via-preloaded-hsts/
- Microsoft Learn, "Support for the .localhost top-level domain"
  (ASP.NET Core) — https://learn.microsoft.com/en-us/aspnet/core/test/localhost-tld
- RFC 6761, "Special-Use Domain Names" — https://en.wikipedia.org/wiki/Special-use_domain_name
- Laravel Valet's DnsMasq implementation (the `.test` + resolver approach
  this deliberately avoids) — https://github.com/laravel/valet/blob/master/cli/Valet/DnsMasq.php
