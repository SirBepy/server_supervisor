// Stub type declaration for the one tauri vendor peer dep this app doesn't
// install: plugin-opener (used by the kit About page's external links).
// plugin-updater is a real dependency now and ships its own types.

declare module "@tauri-apps/plugin-opener" {
  export function openUrl(url: string): Promise<void>;
}
