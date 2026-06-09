import { invoke } from "@tauri-apps/api/core";
import type {
  ProcInfo,
  LogLine,
  Settings,
  Project,
  Command,
  DetectedCommand,
  CommandCheck,
} from "../types/ipc.generated";

// Runtime control (composite "projectId:commandId" ids).
export const listProcs = () => invoke<ProcInfo[]>("list_procs");
export const startProc = (id: string) => invoke<void>("start_proc", { id });
export const stopProc = (id: string) => invoke<void>("stop_proc", { id });
export const restartProc = (id: string) => invoke<void>("restart_proc", { id });
export const reloadProc = (id: string, full = true) =>
  invoke<void>("reload_proc", { id, full });
export const getProcLogs = (id: string) => invoke<LogLine[]>("get_proc_logs", { id });

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
) =>
  invoke<Command>("add_command", {
    projectId,
    name,
    cmd,
    autostart,
    useDynamicPort,
    env,
  });
export const updateCommand = (
  projectId: string,
  commandId: string,
  name: string,
  cmd: string,
  autostart: boolean,
  useDynamicPort: boolean,
  env = "",
) =>
  invoke<Command>("update_command", {
    projectId,
    commandId,
    name,
    cmd,
    autostart,
    useDynamicPort,
    env,
  });
export const removeCommand = (projectId: string, commandId: string) =>
  invoke<void>("remove_command", { projectId, commandId });
export const detectCommands = (path: string) =>
  invoke<DetectedCommand[]>("detect_commands", { path });
// Advisory, non-blocking executable-resolution check (never runs the command).
export function validateCommand(root: string, cmd: string): Promise<CommandCheck> {
  return invoke("validate_command", { root, cmd });
}

export const openInExplorer = (path: string) =>
  invoke<void>("open_in_explorer", { path });

export const getSettings = () => invoke<Settings>("get_settings");
export const getApiToken = () => invoke<string>("get_api_token");
export const quitApp = () => invoke<void>("quit_app");
