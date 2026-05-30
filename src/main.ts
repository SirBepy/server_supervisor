import "@phosphor-icons/web/regular";
import "./styles/base.css";
import { mountDashboard } from "./views/dashboard/dashboard";
import { mountSettings } from "./views/settings/settings";

const app = document.getElementById("app")!;

let cleanup: (() => void) | undefined;

async function route(): Promise<void> {
  cleanup?.();
  cleanup = undefined;
  if (location.hash === "#settings") {
    cleanup = await mountSettings(app);
  } else {
    mountDashboard(app);
  }
}

window.addEventListener("hashchange", () => void route());
void route();
