// Split out from plugins.ts to avoid a circular import: every screen module
// (Topology.tsx, Activity.tsx, ...) needs `PluginScreenProps` to type its
// props, and plugins.ts needs to import those same screen modules to build
// the `PLUGINS` registry — a screen importing its prop type back from
// plugins.ts would be circular. Screens import from here instead.
import type { ComponentType } from "react";
import type { ScopedApi, ScopedSubscribeEvents } from "./scopedApi";
import type { Scope } from "./scopes";

/** What every first-party plugin's screen component receives instead of
 * importing `api`/`subscribeEvents` from lib/api.ts directly — see
 * scopedApi.ts's module doc comment for why that indirection is the whole
 * point. */
export interface PluginScreenProps {
  api: ScopedApi;
  subscribeEvents: ScopedSubscribeEvents;
  /** Switches the nav rail to another registered plugin id — how a
   * screen's quick actions jump to Projects, Databases, Settings, etc. */
  navigate: (screenId: string) => void;
  /** The current theme, and how to change it. Optional because it's an
   * appearance affordance rather than part of the plugin contract —
   * Settings is the only screen that owns the control, now that the nav
   * rail no longer does (a theme switch isn't navigation). */
  theme?: "dark" | "light";
  setTheme?: (theme: "dark" | "light") => void;
}

export interface PluginManifest {
  /** Stable id — used as the React key and the selected-screen state value. */
  id: string;
  /** Nav-rail label — the "contributes a nav entry" contribution point from
   * technical-design.md's plugin architecture section. Only contribution
   * point implemented so far; panel/command contributions are still open
   * (see the task breakdown). */
  navLabel: string;
  /** The real, enforced permission set — see scopedApi.ts. Not advisory. */
  permissions: Scope[];
  component: ComponentType<PluginScreenProps>;
}
