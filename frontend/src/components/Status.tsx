import type { PgState, ServerState } from "../lib/api";

/** Status needs three signals, not one: colour, shape, and text (Carbon).
 *  A crashed process shows its actual exit code, because that specific
 *  detail does more for perceived competence than any icon set — and
 *  `starting` is a first-class state here, not a spinner pretending to be
 *  one of the two stable ones. */
export function Status({ state, compact }: { state: ServerState | PgState; compact?: boolean }) {
  const { cls, label } = describe(state);
  return (
    <span
      style={{
        display: "inline-flex",
        alignItems: "center",
        gap: 6,
        fontSize: compact ? 11 : 11.5,
        color: "var(--ink3)",
      }}
    >
      <span className={`dot ${cls}`} />
      <span className="m">{label}</span>
    </span>
  );
}

function describe(state: ServerState | PgState): { cls: string; label: string } {
  switch (state.status) {
    case "running":
      return { cls: "dot-run", label: "running" };
    case "starting":
      return { cls: "dot-busy", label: "starting" };
    case "stopping":
      return { cls: "dot-busy", label: "stopping" };
    case "crashed":
      return {
        cls: "dot-err",
        // `exit_code` is `number` on ServerState and `number | null` on
        // PgState — a crash we couldn't get a code for says so rather than
        // rendering "exited (null)".
        label: state.exit_code === null || state.exit_code === undefined ? "crashed" : `exited (${state.exit_code})`,
      };
    default:
      return { cls: "dot-stop", label: "stopped" };
  }
}

export function isRunning(state: ServerState | PgState): boolean {
  return state.status === "running";
}

export function isBusy(state: ServerState | PgState): boolean {
  return state.status === "starting" || state.status === "stopping";
}
