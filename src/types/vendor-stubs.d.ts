// Stub type declarations for tauri vendor peer dependencies not used by this app.
// These packages are used by vendor/tauri_kit internals (about page, updater) but
// are not installed because this app does not use those features.

declare module "@tauri-apps/plugin-opener" {
  export function openUrl(url: string): Promise<void>;
}

declare module "@tauri-apps/plugin-updater" {
  export interface Update {
    version: string;
    body?: string;
    downloadAndInstall(onEvent?: (progress: DownloadEvent) => void): Promise<void>;
  }
  export interface DownloadEvent {
    event: string;
    data?: { contentLength?: number; chunkLength?: number };
  }
  export function check(): Promise<Update | null>;
}
