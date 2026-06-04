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
      env: string;
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
      env: string;
      check: CommandCheck | null;
    }
  | {
      t: "confirmDeleteCommand";
      projectId: string;
      commandId: string;
      cmdName: string;
      lastOne: boolean;
    };

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
  // Show only processes that are currently running.
  filterRunning: false,
  // Project IDs that are currently collapsed.
  collapsed: new Set<string>(),
  // Project ID whose per-project "more options" (kebab) menu is open, or null.
  openMenuFor: null as string | null,
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
    const [projs, procs] = await Promise.all([ipc.listProjects(), ipc.listProcs()]);
    ui.projects = projs;
    ui.statusById = Object.fromEntries(procs.map((p) => [p.id, p]));
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
  if (ui.validateTimer !== undefined) {
    window.clearTimeout(ui.validateTimer);
    ui.validateTimer = undefined;
  }
  draw();
}
