// Shared view state + the render trigger for the dashboard. The view modules
// (combobox, modals, dashboard) all read and write through the single `ui`
// object so they observe the same live values, and trigger re-renders through
// `draw()`. dashboard.ts registers the real renderer via `setDraw`, which keeps
// the dependency graph acyclic (no module imports dashboard.ts).

import * as ipc from "../../shared/ipc";
import type {
  ProcInfo,
  Project,
  CommandCheck,
  DetectedCommand,
  Group,
  Role,
  SystemStats,
} from "../../types/ipc.generated";

// Debounce window for the advisory command-validity check.
export const VALIDATE_DEBOUNCE_MS = 350;

// How close (px) to the bottom of the log pane still counts as "at the bottom".
// A small tolerance (~one line) absorbs fractional-pixel rounding so the pane
// stays pinned when the user is effectively at the end.
const LOG_STICK_THRESHOLD_PX = 24;

// True if the open log pane is currently scrolled to (or within a line of) the
// bottom. Read BEFORE new log text is rendered, to decide whether to keep the
// pane pinned. Defaults to true when the pane is not yet in the DOM so a freshly
// opened log starts at the bottom.
function logsAtBottom(): boolean {
  const el = ui.root?.querySelector<HTMLElement>(".logs");
  if (!el) return true;
  return el.scrollHeight - el.scrollTop - el.clientHeight <= LOG_STICK_THRESHOLD_PX;
}

export type PickedCommand = { name: string; cmd: string; ok?: boolean };

export type Modal =
  | null
  | {
      t: "addProject";
      name: string;
      root: string;
      detected: DetectedCommand[];
      picked: PickedCommand[];
      query: string;
      highlight: number;
      existingName: string | null;
    }
  | {
      t: "addCommand";
      projectId: string;
      root: string;
      detected: DetectedCommand[];
      name: string;
      cmd: string;
      useDynamicPort: boolean;
      // Raw text of the port override field. Empty = auto-assign from the
      // project's port block.
      port: string;
      portError: string | null;
      env: string;
      role: Role | null;
      query: string;
      highlight: number;
      check: CommandCheck | null;
    }
  | {
      t: "editCommand";
      projectId: string;
      commandId: string;
      root: string;
      name: string;
      cmd: string;
      autostart: boolean;
      useDynamicPort: boolean;
      // Raw text of the port override field. Empty = auto-assign from the
      // project's port block.
      port: string;
      portError: string | null;
      env: string;
      role: Role | null;
      check: CommandCheck | null;
    }
  | {
      t: "confirmDeleteCommand";
      projectId: string;
      commandId: string;
      cmdName: string;
      lastOne: boolean;
    }
  | {
      t: "renameProject";
      projectId: string;
      name: string;
    };

// Which top-level screen is showing. Home is the default landing screen (stats +
// running-now + the project browser); Project shows one project's commands. This
// is local navigation state (not persisted) - it survives poll refreshes since
// this is a long-lived tray app, so a refresh tick must never bounce the user
// back to Home mid-session.
export type Screen = { t: "home" } | { t: "project"; projectId: string };

// Single mutable view-state object. An object (not module-level `let`s) so that
// imports across modules see the same live values — ES module bindings are
// read-only to importers, but object fields can be mutated by anyone.
export const ui = {
  // The mount element, set by mountDashboard; used as the lit-html render root.
  root: undefined as unknown as HTMLElement,
  projects: [] as Project[],
  statusById: {} as Record<string, ProcInfo>,
  openLogsFor: null as string | null,
  logText: "",
  // Set true right before a draw() that should leave the log pane scrolled to
  // the bottom (on open, or when new lines arrive while already at the bottom).
  // dashboard.ts's draw() consumes and clears it after render.
  scrollLogsToBottom: false,
  error: null as string | null,
  modal: null as Modal,
  // Whether the combobox dropdown is currently shown (driven by input focus).
  comboOpen: false,
  // Debounce handle for the advisory command-validity check.
  validateTimer: undefined as number | undefined,
  // Current top-level screen (Home / Project). See Screen above.
  screen: { t: "home" } as Screen,
  // Command id (`project:command`) selected on the Project screen - drives the
  // row highlight + which command the shared detail pane shows. Kept in lockstep
  // with openLogsFor (below) by the code that sets it, so the existing log-fetch
  // machinery in refresh() needs no changes.
  expandedCmdId: null as string | null,
  // Home's "Running now" list is capped at RUNNING_CAP by default; this flips
  // that to show every match.
  showAllRunning: false,
  // Home's search box query - filters both the running-now list and the project
  // browser by project name or command text.
  homeSearch: "",
  // System-wide RAM/CPU stats for Home's stats strip, polled alongside projects.
  systemStats: null as SystemStats | null,
  // Home's stats-strip donut charts (RAM/CPU): which of the 3 readings ("your
  // apps" %, "system used" %, "total capacity") each ring's center currently
  // shows. Set directly by clicking that arc segment (dashboard.ts's
  // donutPill) - independent per metric, persists across poll refreshes.
  statsDonutMode: { ram: "programs", cpu: "programs" } as Record<"ram" | "cpu", "programs" | "system" | "total">,
  // Project ID whose per-project "more options" (kebab) menu is open, or null.
  openMenuFor: null as string | null,
  // Command id (`project:command`) whose per-command "more options" (kebab) menu
  // is open, or null. Holds the secondary actions (stop/restart, edit/remove);
  // the primary action stays a bare button on the card.
  openCmdMenuFor: null as string | null,
  // Anchor for the floating menu portal. Set when a kebab button is clicked or a
  // context-menu event fires; cleared when the menu is dismissed. The portal
  // renders via position:fixed so it escapes all stacking-context traps.
  menuAnchor: null as { top: number; bottom: number; left: number; right: number; flipUp: boolean } | null,
  // Dashboard density prefs, loaded from settings on mount (see loadPrefs).
  // Defaults mirror the Rust Settings defaults so first paint matches.
  showCommandCount: false,
  showRam: true,
  showPort: true,
  // Per-project resolved icon data URI. undefined = not fetched, null = none
  // found (use tech-logo fallback), string = ready-to-render data URI.
  iconCache: {} as Record<string, string | null | undefined>,
  // Per-project file-detected tech (backend marker scan), the tier-2 fallback
  // when the command name doesn't reveal it. undefined = not fetched, null =
  // unknown, string = a tech key (rust/flutter/node/python/...).
  techCache: {} as Record<string, string | null | undefined>,
  // Groups fetched from the backend each poll tick.
  groups: [] as Group[],
  // Group IDs (and "__other__" for the ungrouped section) that are collapsed.
  // Home's Projects section defaults new groups to collapsed (see refresh):
  // decluttering was the whole point of bringing groups back onto Home.
  collapsedGroups: new Set<string>(),
  // Group IDs we've already applied the default-collapse to, so a poll refresh
  // never re-collapses a group the user has since expanded. Mirrors the
  // seenProjectIds pattern this replaced.
  seenGroupIds: new Set<string>(),
  // Group ID whose kebab menu is open, or null.
  openGroupMenuFor: null as string | null,
  // Project ID currently in "move to group" picker mode, or null.
  openMoveToGroupFor: null as string | null,
  // True when the empty-area right-click menu is open.
  openEmptyMenu: false,
  // Group ID to auto-assign the next successfully-created project to, set by
  // "New project in group" before opening the add-project wizard. Cleared on
  // assignment or modal cancel.
  pendingGroupId: null as string | null,
  // Command ids (`project:command`) whose Project-screen "Environment" block is
  // expanded. Absent = collapsed, which is the default (matches the dev's
  // stated preference: collapsible sections start collapsed, not expanded).
  envSectionOpen: new Set<string>(),
  // `${id}::${key}` pairs whose masked (secret-looking) env value has been
  // clicked to reveal. Absent = masked, which is the default for any key
  // matching TOKEN|SECRET|KEY|PASSWORD|PASSWD|CREDENTIAL (see `EnvVar.secret`,
  // computed once in Rust so the classification isn't duplicated here).
  envRevealed: new Set<string>(),
};

// draw() indirection: dashboard.ts owns the top-level render and registers it
// here, so the view modules can request a re-render without importing it.
let drawFn: () => void = () => {};
export function setDraw(f: () => void) {
  drawFn = f;
}
export function draw() {
  drawFn();
}

export async function refresh() {
  try {
    const [projs, procs, groups, systemStats] = await Promise.all([
      ipc.listProjects(),
      ipc.listProcs(),
      ipc.listGroups(),
      ipc.getSystemStats(),
    ]);
    // Alphabetical by display name (case-insensitive) so the list order is
    // stable and predictable regardless of add order.
    projs.sort((a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: "base" }));
    // First time we see a group, collapse it: the default is closed, and the
    // user expands the one they want. seenGroupIds guards against re-collapsing
    // a group the user later opened (this poll runs every few seconds).
    for (const g of groups) {
      if (!ui.seenGroupIds.has(g.id)) {
        ui.seenGroupIds.add(g.id);
        ui.collapsedGroups.add(g.id);
      }
    }
    ui.projects = projs;
    ui.groups = groups;
    ui.statusById = Object.fromEntries(procs.map((p) => [p.id, p]));
    ui.systemStats = systemStats;
    ui.error = null;
    if (ui.openLogsFor) {
      const lines = await ipc.getProcLogs(ui.openLogsFor);
      // Decide whether to keep the pane pinned BEFORE swapping in the new text:
      // stick to the bottom only if the user is already there (scrolled-up
      // readers are left where they are).
      ui.scrollLogsToBottom = logsAtBottom();
      ui.logText = lines.map((l) => l.text).join("\n");
    }
  } catch (e) {
    ui.error = String(e);
  }
  draw();
}

export async function act(p: Promise<unknown>) {
  try {
    await p;
    ui.error = null;
  } catch (e) {
    ui.error = String(e);
  }
  await refresh();
}

export function closeModal() {
  ui.modal = null;
  ui.pendingGroupId = null;
  if (ui.validateTimer !== undefined) {
    window.clearTimeout(ui.validateTimer);
    ui.validateTimer = undefined;
  }
  draw();
}
