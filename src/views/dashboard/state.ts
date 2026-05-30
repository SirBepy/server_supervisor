// Shared view state + the render trigger for the dashboard. The view modules
// (combobox, modals, dashboard) all read and write through the single `ui`
// object so they observe the same live values, and trigger re-renders through
// `draw()`. dashboard.ts registers the real renderer via `setDraw`, which keeps
// the dependency graph acyclic (no module imports dashboard.ts).

import * as ipc from "../../shared/ipc";
import type {
  ProcInfo,
  Project,
  ProcKind,
  CommandCheck,
  DetectedCommand,
} from "../../types/ipc.generated";

// Debounce window for the advisory command-validity check.
export const VALIDATE_DEBOUNCE_MS = 350;

export type PickedCommand = { name: string; cmd: string; kind: ProcKind; ok?: boolean };

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
      kind: ProcKind;
      useDynamicPort: boolean;
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
      kind: ProcKind;
      autostart: boolean;
      useDynamicPort: boolean;
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
  error: null as string | null,
  modal: null as Modal,
  // Whether the combobox dropdown is currently shown (driven by input focus).
  comboOpen: false,
  // Debounce handle for the advisory command-validity check.
  validateTimer: undefined as number | undefined,
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
