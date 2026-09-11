import { useState } from "react";
import type { PluginScreenProps } from "../lib/pluginTypes";
import { Projects } from "./Projects";
import { ProjectDetail } from "./ProjectDetail";
import { InstanceDetail } from "./InstanceDetail";

/** The one drill-down the whole app is organised around:
 *
 *    projects  →  a project's Odoos  →  one Odoo, everything about it
 *
 *  Each level is a whole screen rather than a pane, deliberately. The design
 *  doc's "design for a narrow window" constraint says the real viewport is
 *  often a ~700-800px column docked beside an editor, and a master-detail
 *  split at that width leaves neither half usable. One level at a time, with
 *  a back button, works at any width.
 *
 *  State lives here rather than in a router because the app is a desktop
 *  window with no URL bar to speak of — and keeping it in React state is
 *  what lets the shell restore exactly where someone was, which the
 *  resumption research (10-15 minutes to rebuild context after an
 *  interruption) makes a real feature rather than a nicety. */
type View =
  | { at: "projects" }
  | { at: "project"; projectId: string }
  | { at: "instance"; projectId: string; serverId: string };

export default function Workspace({ api, subscribeEvents }: PluginScreenProps) {
  const [view, setView] = useState<View>({ at: "projects" });
  // Bumped when returning to the list so counts reflect anything changed
  // deeper in — cheaper and more predictable than keeping a live
  // subscription open on a screen that isn't mounted.
  const [reloadKey, setReloadKey] = useState(0);

  if (view.at === "projects") {
    return <Projects api={api} reloadKey={reloadKey} onOpen={(projectId) => setView({ at: "project", projectId })} />;
  }

  if (view.at === "project") {
    return (
      <ProjectDetail
        api={api}
        subscribeEvents={subscribeEvents}
        projectId={view.projectId}
        onBack={() => {
          setReloadKey((n) => n + 1);
          setView({ at: "projects" });
        }}
        onOpenInstance={(serverId) => setView({ at: "instance", projectId: view.projectId, serverId })}
      />
    );
  }

  return (
    <InstanceDetail
      api={api}
      subscribeEvents={subscribeEvents}
      serverId={view.serverId}
      onBack={() => setView({ at: "project", projectId: view.projectId })}
    />
  );
}
