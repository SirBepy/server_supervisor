// Refused view for dock-pane.ts: rendered for either of two backend
// refusals that both surface as `DockState::Refused` - the app rejected
// `SetParent` outright (`supervisor::dock::headless`'s `refused()` set), or
// it accepted `SetParent` but snapped itself back out of the pane within
// ~200ms (`supervisor::dock::pane_hold`'s own refused set; see todo 0052).
// Either way the app is still on the desktop, so one copy and one view
// covers both causes. Split into its own file rather than growing
// dock-pane.ts further - that file is already past its usual split bar.

import { html, type TemplateResult } from "lit-html";
import * as ipc from "../../shared/ipc";
import { act } from "./state";

export function refusedView(projectId: string, cmdId: string): TemplateResult {
  return html`
    <div class="dockpane-body dockpane-refused">
      <i class="ph ph-eye-slash"></i>
      <p>This app won't stay docked, so it is still on the desktop.</p>
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
