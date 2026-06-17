import "@phosphor-icons/web/regular";
import "./styles/devicon.css";
import "./styles/base.css";
import { mountDashboard } from "./views/dashboard/dashboard";
import { mountSettings } from "./views/settings/settings";
import { bootstrapTheme } from "./shared/theme";
import { runAutoUpdateCheck } from "../vendor/tauri_kit/frontend/updater/auto-check";

const app = document.getElementById("app")!;

let cleanup: (() => void) | undefined;

async function route(): Promise<void> {
  cleanup?.();
  cleanup = undefined;
  // Each view gets a fresh host element that fully replaces the previous one.
  // The two views own their container differently (dashboard renders into it
  // with lit, settings calls replaceChildren on it), so sharing one element
  // leaves stale DOM behind on navigation and corrupts lit's cached part
  // markers. A virgin host per route keeps each view's rendering self-contained.
  const host = document.createElement("div");
  app.replaceChildren(host);
  if (location.hash === "#settings") {
    cleanup = await mountSettings(host);
  } else {
    cleanup = mountDashboard(host);
  }
}

window.addEventListener("hashchange", () => void route());
// Apply the saved/default theme to <html> before (and independent of) the first
// route, so the dashboard paints themed instead of white. The synchronous
// default inside bootstrapTheme lands before route()'s first paint.
void bootstrapTheme();
void route();

// On-startup auto-update check (reads the kit's __kit_auto_update setting;
// default "onStartup" prompts before installing). Skipped under `vite dev`,
// whose binary lags the released version and would falsely "find" an update.
// Note: installing restarts the app, which tree-kills supervised servers - but
// this only fires at a fresh launch, before a work session, so the blast radius
// is minimal (autostart commands relaunch on the new version).
if (!import.meta.env.DEV) void runAutoUpdateCheck();
