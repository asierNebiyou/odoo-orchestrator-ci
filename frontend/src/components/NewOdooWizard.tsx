import { useEffect, useMemo, useState } from "react";
import type { OdooServer } from "../lib/api";
import type { ScopedApi } from "../lib/scopedApi";
import { IconCheck, IconChevronLeft, IconInfo, IconPlus } from "./Icons";

/** The step-by-step "Create new Odoo" flow.
 *
 *  Deliberately one question per step with a Next: the whole primary job is
 *  name it, pick a version, go. Everything a person shouldn't have to know
 *  to get running — which Postgres cluster, which port, what `addons_path`
 *  even is — is decided here and *stated*, never asked. The wizard says what
 *  it picked and why, and offers the escape hatch next to it, which is the
 *  same posture the mockups take on the shared-vs-own-server decision.
 *
 *  The word "server" is avoided throughout in favour of "Odoo": a person
 *  running a client's books does not have a mental model for a server, and
 *  the thing they're making really is just an Odoo. */

const VERSIONS = ["15.0", "16.0", "17.0", "18.0"];

export interface WizardResult {
  server: OdooServer;
  /** Null when the user chose to add code later. */
  addonsPath: string | null;
  /** Null when they chose not to make a database yet. */
  databaseName: string | null;
}

interface Props {
  api: ScopedApi;
  projectId: string;
  projectName: string;
  onCancel: () => void;
  onDone: (result: WizardResult) => void;
}

export function NewOdooWizard({ api, projectId, projectName, onCancel, onDone }: Props) {
  const [step, setStep] = useState(0);
  const [name, setName] = useState("");
  const [version, setVersion] = useState("17.0");
  const [codeChoice, setCodeChoice] = useState<"none" | "folder" | "git">("none");
  const [addonsPath, setAddonsPath] = useState("");
  // One database is the default shape. Adding more here is what turns
  // this into "one Odoo, several databases", which Odoo routes between by
  // hostname — see the topology rule in the runtime-architecture doc.
  const [extraDatabases, setExtraDatabases] = useState<string[]>([]);
  const [databaseName, setDatabaseName] = useState("");

  const [usedPorts, setUsedPorts] = useState<number[]>([]);
  const [working, setWorking] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    api
      .listServers()
      .then((servers) => {
        if (!alive) return;
        setUsedPorts(servers.map((s) => s.port));
      })
      .catch((err) => alive && setError(String(err)));
    return () => {
      alive = false;
    };
  }, [api]);

  // Escape backs out of the whole wizard, but never mid-create — cancelling
  // a keystroke after the Odoo row is written would leave a half-made thing
  // with no way back to it.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !working) onCancel();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onCancel, working]);

  const port = useMemo(() => {
    let candidate = 8069;
    while (usedPorts.includes(candidate)) candidate += 1;
    return candidate;
  }, [usedPorts]);

  // Whether this version is already on the machine, so the step can say
  // "reused" instead of silently downloading a gigabyte later.
  const [have, setHave] = useState<boolean | null>(null);
  useEffect(() => {
    let alive = true;
    setHave(null);
    api
      .findOdoo(version)
      .then((r) => alive && setHave(r.found))
      .catch(() => alive && setHave(null));
    return () => {
      alive = false;
    };
  }, [api, version]);

  const slug = useMemo(() => toSlug(name), [name]);
  const dbName = useMemo(() => (databaseName.trim() ? toSlug(databaseName) : slug), [databaseName, slug]);

  const steps = ["Name", "Version", "Your modules"];
  const canAdvance =
    (step === 0 && slug.length > 0) ||
    step === 1 ||
    (step === 2 && (codeChoice === "none" || addonsPath.trim().length > 0));

  async function create() {
    setError(null);
    try {
      // Real named stages, because these genuinely happen one after
      // another and any of them can fail on its own.
      setWorking("Setting it up");
      const server = await api.createServer(name.trim(), version, port, projectId);

      if (codeChoice !== "none" && addonsPath.trim()) {
        setWorking(isGitUrl(addonsPath) ? "Cloning your modules" : "Registering your modules");
        await api.createAddonsSource(server.id, "My modules", addonsPath.trim(), "private", 0);
      }

      // Always at least one: an Odoo with no database is nothing you can
      // log into.
      setWorking(`Creating the database ${dbName}`);
      const { database } = await api.createDatabase(server.id, dbName);

      for (const extra of extraDatabases.map(toSlug).filter((n) => n && n !== dbName)) {
        setWorking(`Creating the database ${extra}`);
        await api.createDatabase(server.id, extra);
      }

      onDone({ server, addonsPath: codeChoice !== "none" ? addonsPath.trim() : null, databaseName: database.name });
    } catch (err) {
      setWorking(null);
      setError(describeError(err));
    }
  }

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label="Create a new Odoo"
      onClick={(e) => {
        if (e.target === e.currentTarget && !working) onCancel();
      }}
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(5,5,7,0.74)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        padding: 40,
        zIndex: 60,
      }}
    >
      <div
        style={{
          width: 560,
          maxWidth: "100%",
          background: "var(--s1)",
          borderRadius: 12,
          boxShadow: "var(--shadow-pop)",
          overflow: "hidden",
        }}
      >
        <div style={{ padding: "22px 24px 0" }}>
          <div style={{ fontSize: 17, fontWeight: 600, letterSpacing: "-0.02em", marginBottom: 6 }}>
            New Odoo in {projectName}
          </div>
          <div style={{ fontSize: 12.5, color: "var(--ink3)", lineHeight: 1.55 }}>
            {[
              "What should it be called?",
              "Which Odoo version?",
              "Where are your own modules?",
            ][step]}
          </div>

          {/* Endowed progress: the step dots show the whole path up front, so
              the last step reads as nearly-done rather than open-ended. */}
          <div style={{ display: "flex", gap: 6, marginTop: 16 }}>
            {steps.map((label, i) => (
              <div key={label} style={{ flex: 1 }}>
                <div
                  style={{
                    height: 3,
                    borderRadius: 2,
                    background: i < step ? "var(--ok)" : i === step ? "var(--acc)" : "var(--hair2)",
                    transition: "background-color 0.15s ease-out",
                  }}
                />
                <div
                  style={{
                    fontSize: 10,
                    marginTop: 6,
                    color: i === step ? "var(--ink2)" : "var(--ink4)",
                    letterSpacing: 0.02,
                  }}
                >
                  {label}
                </div>
              </div>
            ))}
          </div>
        </div>

        <div style={{ padding: "22px 24px 0", minHeight: 214 }}>
          {step === 0 ? (
            <div>
              <div className="cap" style={{ marginBottom: 8 }}>
                Name
              </div>
              <input
                className="field"
                autoFocus
                value={name}
                placeholder="Acme Retail"
                onChange={(e) => setName(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && slug) setStep(1);
                }}
              />
              <div style={{ fontSize: 11.5, color: "var(--ink4)", marginTop: 8, lineHeight: 1.55 }}>
                {slug ? (
                  <>
                    Its databases will be reachable at{" "}
                    <span className="m" style={{ color: "var(--acc-ink)" }}>
                      {slug}.localhost
                    </span>
                  </>
                ) : (
                  "Anything you'll recognise later — the client, the project, the experiment."
                )}
              </div>
            </div>
          ) : null}

          {step === 1 ? (
            <div>
              <div className="cap" style={{ marginBottom: 8 }}>
                Odoo version
              </div>
              <div className="segtrack">
                {VERSIONS.map((v) => (
                  <button key={v} className={`seg m${version === v ? " on" : ""}`} onClick={() => setVersion(v)}>
                    {v}
                  </button>
                ))}
              </div>
              <InfoNote>
                {have === null
                  ? "Checking whether this machine already has this version…"
                  : have
                    ? `Already on this machine — it'll be reused, nothing to download.`
                    : "This machine doesn't have this version yet, so the first start downloads it once. Every project that wants this version then shares the same copy."}
              </InfoNote>
            </div>
          ) : null}

          {step === 2 ? (
            <div>
              <div className="cap" style={{ marginBottom: 8 }}>
                Your modules
              </div>
              <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
                <Choice
                  on={codeChoice === "none"}
                  title="None yet — just Odoo's own apps"
                  detail="Sales, invoicing, inventory and the rest of the standard apps. You can add your own any time."
                  onPick={() => setCodeChoice("none")}
                />
                <Choice
                  on={codeChoice === "folder"}
                  title="A folder on this machine"
                  detail="A checkout you already maintain, or a client's customisations."
                  onPick={() => setCodeChoice("folder")}
                />
                <Choice
                  on={codeChoice === "git"}
                  title="A repository"
                  detail="Cloned with whatever access this machine already has — your SSH key, git's credential helper, or gh."
                  onPick={() => setCodeChoice("git")}
                />
              </div>
              {codeChoice !== "none" ? (
                <input
                  className="field"
                  style={{ marginTop: 10 }}
                  autoFocus
                  value={addonsPath}
                  placeholder={
                    codeChoice === "git"
                      ? "git@github.com:acme/acme-addons.git"
                      : "/Users/you/work/acme/addons"
                  }
                  onChange={(e) => setAddonsPath(e.target.value)}
                />
              ) : null}

              <div style={{ marginTop: 18 }}>
                <div className="cap" style={{ marginBottom: 8 }}>
                  {extraDatabases.length ? "Its databases" : "Its database"}
                </div>
                <input
                  className="field"
                  value={databaseName}
                  placeholder={slug}
                  onChange={(e) => setDatabaseName(e.target.value)}
                />
                <div style={{ fontSize: 11.5, color: "var(--ink4)", marginTop: 8 }}>
                  You'll log in at{" "}
                  <span className="m" style={{ color: "var(--acc-ink)" }}>
                    {dbName}.localhost
                  </span>
                </div>

                {extraDatabases.map((extra, i) => (
                  <div key={i} style={{ display: "flex", gap: 8, marginTop: 8, alignItems: "center" }}>
                    <input
                      className="field"
                      value={extra}
                      placeholder="another_database"
                      onChange={(e) =>
                        setExtraDatabases((list) => list.map((v, j) => (j === i ? e.target.value : v)))
                      }
                    />
                    <button
                      className="btn btn-q btn-s"
                      onClick={() => setExtraDatabases((list) => list.filter((_, j) => j !== i))}
                    >
                      Remove
                    </button>
                  </div>
                ))}

                <button
                  className="btn btn-q btn-s"
                  style={{ marginTop: 10 }}
                  onClick={() => setExtraDatabases((list) => [...list, ""])}
                >
                  <IconPlus size={12} strokeWidth={2.2} />
                  Add another database
                </button>
                {extraDatabases.length ? (
                  <div style={{ fontSize: 11.5, color: "var(--ink4)", marginTop: 8, lineHeight: 1.55 }}>
                    They'll share this one Odoo — same version, same modules — each on its own address. That's
                    cheapest, and they start together. A database that needs a different version or different modules
                    needs its own Odoo instead.
                  </div>
                ) : null}
              </div>
            </div>
          ) : null}

          {error ? (
            <div
              style={{
                marginTop: 14,
                background: "var(--err-soft)",
                border: "1px solid oklch(0.67 0.18 22 / 0.3)",
                borderRadius: 9,
                padding: "11px 13px",
                fontSize: 11.5,
                color: "var(--ink2)",
                lineHeight: 1.55,
              }}
            >
              {error}
            </div>
          ) : null}
        </div>

        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            padding: "20px 24px",
            marginTop: 22,
            borderTop: "1px solid var(--hair)",
            background: "var(--s2)",
          }}
        >
          <span style={{ fontSize: 11.5, color: "var(--ink4)" }}>
            {working ? (
              <span style={{ display: "inline-flex", alignItems: "center", gap: 8, color: "var(--ink3)" }}>
                <span className="dot dot-busy" />
                {working}…
              </span>
            ) : (
              `Step ${step + 1} of ${steps.length}`
            )}
          </span>
          <div style={{ display: "flex", gap: 9 }}>
            {step > 0 && !working ? (
              <button className="btn btn-q" onClick={() => setStep(step - 1)}>
                <IconChevronLeft size={13} />
                Back
              </button>
            ) : null}
            {!working ? (
              <button className="btn btn-q" onClick={onCancel}>
                Cancel
              </button>
            ) : null}
            {step < steps.length - 1 ? (
              <button className="btn btn-p" disabled={!canAdvance} onClick={() => setStep(step + 1)}>
                Next
              </button>
            ) : (
              <button className="btn btn-p" disabled={!!working || !canAdvance} onClick={create}>
                Create {name.trim() || "it"}
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

function Choice({
  on,
  title,
  detail,
  onPick,
}: {
  on: boolean;
  title: string;
  detail: string;
  onPick: () => void;
}) {
  return (
    <button
      onClick={onPick}
      style={{
        textAlign: "left",
        display: "flex",
        alignItems: "flex-start",
        gap: 12,
        background: on ? "var(--acc-soft)" : "var(--s2)",
        border: `1px solid ${on ? "var(--acc-line)" : "var(--hair)"}`,
        borderRadius: 9,
        padding: "13px 15px",
        cursor: "pointer",
        color: "inherit",
        font: "inherit",
        transition: "background-color 0.12s ease-out, border-color 0.12s ease-out",
      }}
    >
      <span style={{ color: on ? "var(--acc-ink)" : "var(--ink4)", display: "flex", marginTop: 1 }}>
        {on ? (
          <IconCheck size={15} />
        ) : (
          <span
            style={{
              width: 15,
              height: 15,
              borderRadius: "50%",
              border: "1.5px solid var(--hair2)",
              display: "block",
            }}
          />
        )}
      </span>
      <span style={{ flex: 1 }}>
        <span style={{ display: "block", fontSize: 12.5, color: "var(--ink)", marginBottom: 3 }}>{title}</span>
        <span style={{ display: "block", fontSize: 11.5, color: "var(--ink3)", lineHeight: 1.55 }}>{detail}</span>
      </span>
    </button>
  );
}

function InfoNote({ children }: { children: React.ReactNode }) {
  return (
    <div
      style={{
        marginTop: 14,
        display: "flex",
        alignItems: "flex-start",
        gap: 11,
        background: "var(--s2)",
        border: "1px solid var(--hair)",
        borderRadius: 9,
        padding: "12px 14px",
      }}
    >
      <span style={{ color: "var(--ink4)", display: "flex", marginTop: 1 }}>
        <IconInfo size={14} />
      </span>
      <div style={{ fontSize: 11.5, color: "var(--ink3)", lineHeight: 1.6 }}>{children}</div>
    </div>
  );
}

/** Odoo database names have to survive being a Postgres identifier and a
 *  hostname label, so this is stricter than a display name needs to be. */
function toSlug(input: string): string {
  return input
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "")
    .slice(0, 48);
}

function isGitUrl(value: string): boolean {
  const v = value.trim();
  return v.startsWith("https://") || v.startsWith("http://") || v.startsWith("git@") || v.startsWith("ssh://");
}

export function describeError(err: unknown): string {
  const raw = err instanceof Error ? err.message : String(err);
  if (raw.includes("409")) return "Something with that name or port already exists. Try a different name.";
  if (raw.includes("failed: 500")) return "Something went wrong setting that up. The details are in this Odoo's Logs tab.";
  return raw;
}
