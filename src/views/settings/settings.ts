import { html, render } from "lit-html";
import { renderSettingsPage } from "../../../vendor/tauri_kit/frontend/settings/renderer";
import "../../../vendor/tauri_kit/frontend/settings/styles.css";
import { getApiToken } from "../../shared/ipc";
import { buildSettingsSchema } from "./schema";
import "./settings.css";

export async function mountSettings(el: HTMLElement): Promise<void> {
  const token = await getApiToken().catch(() => "(unavailable)");

  const headerEl = document.createElement("header");
  headerEl.className = "settings-topbar";
  const contentEl = document.createElement("div");
  el.replaceChildren(headerEl, contentEl);

  function renderHeader(title: string, onBack: () => void): void {
    render(
      html`
        <button class="icon-btn" title="Back" @click=${onBack}>
          <i class="ph ph-arrow-left"></i>
        </button>
        <span class="settings-title">${title}</span>
        <span class="spacer"></span>
      `,
      headerEl,
    );
  }

  renderHeader("Settings", () => {
    location.hash = "#dashboard";
  });

  await renderSettingsPage(contentEl, {
    schema: buildSettingsSchema(token),
    onHeaderChange(title, depth, pop) {
      if (depth === 0) {
        renderHeader("Settings", () => {
          location.hash = "#dashboard";
        });
      } else {
        renderHeader(title, pop);
      }
    },
  });
}
