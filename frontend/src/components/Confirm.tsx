import { useEffect, useRef, useState } from "react";
import { IconWarn } from "./Icons";
import { autoSnapshotBeforeDestructive } from "../lib/format";

/** The friction ladder from `docs/odoo-orchestrator-ui-design-principles.md`,
 *  keyed to blast radius rather than applied uniformly.
 *
 *  Uniform "Are you sure?" dialogs are measurably **worse than none** — the
 *  CHI 2015 / MISQ 2018 fMRI work found visual processing collapses after
 *  only the *second* exposure to a repeated warning, training an autopilot
 *  dismissal that carries into the one dialog that mattered. So this looks
 *  deliberately different per rung (2 plain, 3 with a pre-checked safety
 *  snapshot, 4 type-to-confirm) and rung 0-1 actions never open it at all.
 *
 *  Copy needs four elements or it's decorative: named object, quantified
 *  consequence, reversibility status, ripple effects. The button says the
 *  action ("Drop acme_test"), never "Confirm".
 *
 *  Chrome follows the mockups' modal exactly (560px, 12px radius, ring-then-
 *  shadow, s2 footer bar). */
export interface ConfirmSpec {
  rung: 2 | 3 | 4;
  title: string;
  /** What exactly gets touched. Rendered monospace, and at rung 4 it's the
   *  string that has to be typed back. */
  object: string;
  consequence: string;
  reversibility: string;
  ripple?: string;
  /** The action, not "Confirm". */
  actionLabel: string;
  /** Rung 3's pre-checked safety snapshot — the highest-leverage move in
   *  the whole design doc, because it converts the core fear into a solved,
   *  visible fact at the exact moment the fear occurs. */
  offerCounterSnapshot?: boolean;
  onConfirm: (opts: { takeCounterSnapshot: boolean }) => void;
}

/** `onClose` dismisses the dialog, and this component calls it itself on
 *  *both* paths — cancel and confirm. That is deliberate: when closing on
 *  confirm was each caller's job, one screen did it and the other forgot,
 *  so confirming a restore on an Odoo left the modal sitting over the app
 *  with its danger button still live and still clickable. A guard that can
 *  be fired twice is not a guard. Callers show progress in the row they
 *  changed, the way the Settings screen already did. */
export function ConfirmDialog({ spec, onClose }: { spec: ConfirmSpec; onClose: () => void }) {
  const [typed, setTyped] = useState("");
  // `offerCounterSnapshot` says this action *can* take one; whether the
  // box starts ticked is the user's setting. Before this, the setting was
  // written by the Settings screen and read by nothing — a control that
  // did nothing, which is worse than no control.
  const [takeCounterSnapshot, setTakeCounterSnapshot] = useState(
    (spec.offerCounterSnapshot ?? false) && autoSnapshotBeforeDestructive(),
  );
  const cancelRef = useRef<HTMLButtonElement>(null);

  // Escape dismisses safely, and focus lands on Cancel — the destructive
  // button is never the one Enter reaches.
  useEffect(() => {
    cancelRef.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const satisfied = spec.rung !== 4 || typed === spec.object;

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label={spec.title}
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
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
          width: 520,
          maxWidth: "100%",
          background: "var(--s1)",
          borderRadius: 12,
          boxShadow: "var(--shadow-pop)",
          overflow: "hidden",
        }}
      >
        <div style={{ padding: "22px 24px 0" }}>
          <div style={{ display: "flex", alignItems: "center", gap: 10, marginBottom: 6 }}>
            <span style={{ color: "var(--err)", display: "flex" }}>
              <IconWarn size={16} />
            </span>
            <span style={{ fontSize: 17, fontWeight: 600, letterSpacing: "-0.02em" }}>{spec.title}</span>
          </div>
          <div style={{ fontSize: 12.5, color: "var(--ink3)", lineHeight: 1.55 }}>
            <span className="m" style={{ color: "var(--ink)" }}>
              {spec.object}
            </span>{" "}
            — {spec.consequence}
          </div>
        </div>

        <div style={{ padding: "18px 24px 0", display: "flex", flexDirection: "column", gap: 12 }}>
          <div
            style={{
              background: "var(--s2)",
              border: "1px solid var(--hair)",
              borderRadius: 9,
              padding: "13px 15px",
              display: "flex",
              flexDirection: "column",
              gap: 7,
              fontSize: 11.5,
              lineHeight: 1.55,
              color: "var(--ink3)",
            }}
          >
            <div>{spec.reversibility}</div>
            {spec.ripple ? <div>{spec.ripple}</div> : null}
          </div>

          {spec.offerCounterSnapshot ? (
            <label
              style={{
                background: "var(--ok-soft)",
                border: "1px solid oklch(0.72 0.15 155 / 0.28)",
                borderRadius: 9,
                padding: "13px 15px",
                display: "flex",
                alignItems: "flex-start",
                gap: 12,
                cursor: "pointer",
              }}
            >
              <input
                type="checkbox"
                checked={takeCounterSnapshot}
                onChange={(e) => setTakeCounterSnapshot(e.target.checked)}
                style={{ marginTop: 1, accentColor: "var(--ok)" }}
              />
              <div style={{ flex: 1 }}>
                <div style={{ fontSize: 12.5, color: "var(--ink)", marginBottom: 3 }}>
                  Snapshot the current data first
                </div>
                <div style={{ fontSize: 11.5, color: "var(--ink3)", lineHeight: 1.55 }}>
                  Takes a frozen copy before anything is overwritten, so this is undoable in one click.
                </div>
              </div>
            </label>
          ) : null}

          {spec.rung === 4 ? (
            <div>
              <div className="cap" style={{ marginBottom: 8 }}>
                Type the name to confirm
              </div>
              <input
                className="field"
                value={typed}
                autoFocus
                spellCheck={false}
                autoComplete="off"
                placeholder={spec.object}
                onChange={(e) => setTyped(e.target.value)}
              />
            </div>
          ) : null}
        </div>

        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "flex-end",
            gap: 9,
            padding: "20px 24px",
            marginTop: 22,
            borderTop: "1px solid var(--hair)",
            background: "var(--s2)",
          }}
        >
          <button ref={cancelRef} className="btn btn-q" onClick={onClose}>
            Cancel
          </button>
          <button
            className="btn btn-danger"
            disabled={!satisfied}
            onClick={() => {
              // Closed first, so the action can never be fired twice while
              // the work it started is still running.
              onClose();
              spec.onConfirm({ takeCounterSnapshot });
            }}
          >
            {spec.actionLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
