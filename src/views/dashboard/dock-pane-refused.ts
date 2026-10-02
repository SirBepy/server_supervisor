// Refused-headless view for dock-pane.ts: rendered when the backend tried
// SetParent on this command's window and the app rejected it (see
// supervisor::dock::headless's `refused()` set, surfaced as
// `DockState::Refused`). Split into its own file rather than growing
// dock-pane.ts further - that file is already past its usual split bar.

import { html, type TemplateResult } from "lit-html";
import * as ipc from "../../shared/ipc";
import { act } from "./state";

export function refusedView(projectId: string, cmdId: string): TemplateResult {
  return html`
    <div class="dockpane-body dockpane-refused">
      <i class="ph ph-eye-slash"></i>
      <p>This app refused to run headless; it is still on the desktop.</p>
      <button
        class="abtn"
        title="Stop trying to run it headless"
        @click=${() => void act(ipc.setCommandHeadless(projectId, cmdId, false))}
      >
        <i class="ph ph-app-window"></i> Show window normally
      </button>
    </div>
  `;
}
