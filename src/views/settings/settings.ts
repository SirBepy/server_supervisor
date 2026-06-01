import { html, render } from "lit-html";
import { renderSettingsPage } from "../../../vendor/tauri_kit/frontend/settings/renderer";
import "../../../vendor/tauri_kit/frontend/settings/styles.css";
import { getApiToken } from "../../shared/ipc";
import { PALETTES, DEFAULT_PALETTE } from "../../shared/theme";
import { buildSettingsSchema } from "./schema";
import "./settings.css";

export async function mountSettings(el: HTMLElement): Promise<() => void> {
  const token = await getApiToken().catch(() => "(unavailable)");

  // The kit's settings UI lays its pages out with position:absolute; height:100%,
  // so it needs a mount container with a definite height. Make this view a
  // full-height flex column (header natural, content fills the rest) so the
  // kit's pages resolve their 100% height instead of collapsing to 0 (which
  // renders the content invisible).
  el.classList.add("settings-view");
  const headerEl = document.createElement("header");
  headerEl.className = "settings-topbar";
  const contentEl = document.createElement("div");
  contentEl.className = "settings-content";
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

  const cleanup = await renderSettingsPage(contentEl, {
    schema: buildSettingsSchema(token),
    palettes: PALETTES,
    // Default palette is shared with the boot path (see shared/theme.ts) so the
    // first-paint default and the picker default never drift. Mode left at the
    // kit default ("system").
    theme: { defaultPalette: DEFAULT_PALETTE },
    onHeaderChange(title, depth, pop) {
      // PageStack reports depth as the 1-based stack length, so the root page is
      // depth 1 (never 0). At the root, Back must exit settings to the dashboard;
      // only deeper pages pop the stack. Checking `depth === 0` here left the root
      // Back button wired to pop(), which is a no-op at the root — a dead button.
      if (depth <= 1) {
        renderHeader("Settings", () => {
          location.hash = "#dashboard";
        });
      } else {
        renderHeader(title, pop);
      }
    },
  });
  return cleanup;
}
