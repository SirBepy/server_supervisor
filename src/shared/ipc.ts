import { invoke } from "@tauri-apps/api/core";
import type {
  ProcInfo,
  LogLine,
  Settings,
  Project,
  Command,
  CommandParam,
  DetectedCommand,
  CommandCheck,
  ProjectIcon,
  Group,
  VersionInfo,
  Role,
  SystemStats,
  UpstreamPreset,
  RequestLogEntry,
  DockRect,
  DockOutcome,
  DockState,
} from "../types/ipc.generated";

// Runtime control (composite "projectId:commandId" ids).
export const listProcs = () => invoke<ProcInfo[]>("list_procs");
export const startProc = (id: string) => invoke<void>("start_proc", { id });
export const stopProc = (id: string) => invoke<void>("stop_proc", { id });
export const restartProc = (id: string) => invoke<void>("restart_proc", { id });
export const reloadProc = (id: string, full = true) =>
  invoke<void>("reload_proc", { id, full });
export const getProcLogs = (id: string) => invoke<LogLine[]>("get_proc_logs", { id });
// Open a command's port in a browser. flutter=true routes it to the dedicated
// CORS-disabled dev browser (new tab in the same window); else the default browser.
export const openPortUrl = (url: string, flutter: boolean) =>
  invoke<void>("open_port_url", { url, flutter });

// Native window docking (see supervisor::dock). `rect` is the dashboard's
// reserved-hole bounds in screen PHYSICAL pixels (see dock-pane.ts for the
// getBoundingClientRect -> screen-px conversion) - the backend positions the
// process's real OS window over it, it never draws anything itself.
export const dockProcWindow = (id: string, rect: DockRect) =>
  invoke<DockOutcome>("dock_proc_window", { id, rect });
export const undockProcWindow = (id: string) => invoke<void>("undock_proc_window", { id });
// Re-asserts an already-docked window's bounds (pane resize/scroll/collapse).
// A no-op backend-side if `id` isn't actively docked.
export const setProcDockBounds = (id: string, rect: DockRect) =>
  invoke<void>("set_proc_dock_bounds", { id, rect });
export const getDockState = (id: string) => invoke<DockState>("get_dock_state", { id });
// PNG of the proc's window as a data: URL, the same image the HTTP API's
// /screenshot hands an agent.
export const captureProcWindow = (id: string) => invoke<string>("capture_proc_window", { id });

// Project / command config CRUD.
export const listProjects = () => invoke<Project[]>("list_projects");
export const addProject = (name: string, root: string) =>
  invoke<Project>("add_project", { name, root });
export const removeProject = (projectId: string) =>
  invoke<void>("remove_project", { projectId });
export const renameProject = (projectId: string, name: string) =>
  invoke<Project>("rename_project", { projectId, name });
export const addCommand = (
  projectId: string,
  name: string,
  cmd: string,
  autostart: boolean,
  useDynamicPort: boolean,
  env = "",
  role: Role | null = null,
  // Manual port override; null = auto-assign from the project's port block.
  fixedPort: number | null = null,
  // Whether to dock this command's window into the dashboard. Sent on every
  // call (like autostart/useDynamicPort above) - the backend treats an
  // omitted key as false, so a caller that forgets it silently clears docking.
  dockWindow = false,
  // Named axes authored via the Parameters section. Optional trailing arg so
  // every existing call site (add-project.ts's bulk picker included) keeps
  // compiling unchanged; an omitted params list is the same as "no params".
  params: CommandParam[] = [],
) =>
  invoke<Command>("add_command", {
    projectId,
    name,
    cmd,
    autostart,
    useDynamicPort,
    fixedPort,
    env,
    role,
    dockWindow,
    params,
  });
export const updateCommand = (
  projectId: string,
  commandId: string,
  name: string,
  cmd: string,
  autostart: boolean,
  useDynamicPort: boolean,
  env = "",
  role: Role | null = null,
  // Manual port override; null = auto-assign from the project's port block.
  fixedPort: number | null = null,
  // See addCommand's dockWindow above: this is a full-replace endpoint, so
  // every call must send the caller's current intent, never omit it.
  dockWindow = false,
  // Named axes authored via the Parameters section. Omitted (undefined) ->
  // sent as `null`, which the backend reads as "keep the existing params" -
  // distinct from an explicit `[]`, which clears them (the edit modal always
  // passes its current full list, so either outcome is reachable).
  params?: CommandParam[],
) =>
  invoke<Command>("update_command", {
    projectId,
    commandId,
    name,
    cmd,
    autostart,
    useDynamicPort,
    fixedPort,
    env,
    role,
    dockWindow,
    params: params ?? null,
  });
export const removeCommand = (projectId: string, commandId: string) =>
  invoke<void>("remove_command", { projectId, commandId });
// Live toggles: apply to a running command without restarting it.
export const setCommandSound = (projectId: string, commandId: string, on: boolean) =>
  invoke<Command>("set_command_sound", { projectId, commandId, on });
export const setCommandHeadless = (projectId: string, commandId: string, on: boolean) =>
  invoke<Command>("set_command_headless", { projectId, commandId, on });
// Per-param value picker (Project-screen kebab): persists `last_value` and
// restarts the command if it's currently running.
export const setCommandParam = (projectId: string, commandId: string, name: string, valueId: string) =>
  invoke<Command>("set_command_param", { projectId, commandId, name, valueId });
// "Combine into one command" migration: merges 2+ existing commands (by id,
// in the dev's picked order) into one new parameterized command and removes
// the sources. `param.values` must have exactly one entry per source id, in
// the same order.
export const combineCommands = (
  projectId: string,
  sourceIds: string[],
  name: string,
  template: string,
  param: CommandParam,
) => invoke<Command>("combine_commands", { projectId, sourceIds, name, template, param });

// Reverse-proxy hub: one fixed loopback listener per project, forwarding to
// whichever upstream preset is active. See supervisor::proxy_hub.
export const getHubPort = (projectId: string) =>
  invoke<number>("get_hub_port", { projectId });
export const addPreset = (projectId: string, name: string, baseUrl: string, danger = false) =>
  invoke<UpstreamPreset>("add_preset", { projectId, name, baseUrl, danger });
export const removePreset = (projectId: string, presetId: string) =>
  invoke<void>("remove_preset", { projectId, presetId });
export const setActivePreset = (projectId: string, presetId: string) =>
  invoke<void>("set_active_preset", { projectId, presetId });
export const getHubLog = (projectId: string) =>
  invoke<RequestLogEntry[]>("get_hub_log", { projectId });
export const detectCommands = (path: string) =>
  invoke<DetectedCommand[]>("detect_commands", { path });
// Advisory, non-blocking executable-resolution check (never runs the command).
export function validateCommand(root: string, cmd: string): Promise<CommandCheck> {
  return invoke("validate_command", { root, cmd });
}

export const openInExplorer = (path: string) =>
  invoke<void>("open_in_explorer", { path });

export const getProjectIcon = (root: string) =>
  invoke<ProjectIcon | null>("get_project_icon", { root });

export const getProjectTech = (root: string) =>
  invoke<string | null>("get_project_tech", { root });

export const getSettings = () => invoke<Settings>("get_settings");
export const getSystemStats = () => invoke<SystemStats>("get_system_stats");
export const getApiToken = () => invoke<string>("get_api_token");
export const quitApp = () => invoke<void>("quit_app");
// About page build/install info (kit's lazy `getVersionInfo` option).
export const getVersionInfo = () => invoke<VersionInfo>("get_version_info");

// Group CRUD
export const listGroups = () => invoke<Group[]>("list_groups");
export const createGroup = (name: string) => invoke<Group>("create_group", { name });
export const updateGroup = (id: string, name: string) =>
  invoke<Group>("update_group", { id, name });
export const deleteGroup = (id: string) => invoke<void>("delete_group", { id });
export const setProjectGroup = (projectId: string, groupId: string | null) =>
  invoke<void>("set_project_group", { projectId, groupId });
