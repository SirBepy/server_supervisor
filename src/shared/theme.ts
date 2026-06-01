import { invoke } from "@tauri-apps/api/core";
import {
  applyTheme,
  DEFAULT_MODE,
  type ThemeValue,
} from "../../vendor/tauri_kit/frontend/settings/pages/theme";
import { SIRBEPY_PALETTES } from "../../vendor/tauri_kit/frontend/settings/palettes/sirbepy-default";
import "../../vendor/tauri_kit/frontend/settings/palettes/sirbepy-default.css";

// Single source of truth for this app's theme config, shared by the boot path
// (bootstrapTheme, below) and the Settings page (settings.ts). Keeping the
// default palette here means the first-paint default can never drift from what
// the settings picker treats as default.
export const PALETTES = SIRBEPY_PALETTES;
// Glacier (blue): reads infrastructure/terminal, distinct from the other apps.
export const DEFAULT_PALETTE = "glacier";

// The kit persists the active mode/palette under these keys in get_settings.
const MODE_KEY = "__kit_theme";
const PALETTE_KEY = "__kit_palette";

/**
 * Apply the persisted (or default) palette + mode to <html> once at app boot.
 * The dashboard is the default route and used to mount with no theme applied,
 * leaving <html> without data-theme/data-mode so every palette CSS var was
 * undefined and the whole app painted white until Settings was opened (which
 * was the only caller of applyTheme). This makes theming an app-level concern.
 */
export async function bootstrapTheme(): Promise<void> {
  // Synchronous so the very first paint is already themed (no white flash).
  applyTheme(DEFAULT_MODE, DEFAULT_PALETTE);
  try {
    const s = (await invoke<Record<string, unknown>>("get_settings")) ?? {};
    const mode = (s[MODE_KEY] as ThemeValue) ?? DEFAULT_MODE;
    const palette = (s[PALETTE_KEY] as string) ?? DEFAULT_PALETTE;
    applyTheme(mode, palette);
  } catch {
    // No saved settings yet, or IPC unavailable (e.g. running in a plain
    // browser): the synchronous default above stays applied.
  }
}
