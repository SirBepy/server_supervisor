import { invoke } from "@tauri-apps/api/core";
import type { ProcInfo, LogLine, Settings } from "../types/ipc.generated";

export const listProcs = () => invoke<ProcInfo[]>("list_procs");
export const startProc = (id: string) => invoke<void>("start_proc", { id });
export const stopProc = (id: string) => invoke<void>("stop_proc", { id });
export const restartProc = (id: string) => invoke<void>("restart_proc", { id });
export const reloadProc = (id: string, full = true) =>
  invoke<void>("reload_proc", { id, full });
export const getProcLogs = (id: string) => invoke<LogLine[]>("get_proc_logs", { id });

export const getSettings = () => invoke<Settings>("get_settings");
export const quitApp = () => invoke<void>("quit_app");
