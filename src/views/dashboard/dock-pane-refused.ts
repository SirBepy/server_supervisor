// Refused view for dock-pane.ts: rendered for either of two backend
// refusals that both surface as `DockState::Refused` - the app rejected
// `SetParent` outright (`supervisor::dock::headless`'s `refused()` set), or
// it accepted `SetParent` but snapped itself back out of the pane within
// ~200ms (`supervisor::dock::pane_hold`'s own refused set; see todo 0052).
// Either way the app is still on the desktop, so one sentence covers both
// causes. The frontend only learns `refused`, not which set it came from;
// `dock_headless` stands in for the cause, since a headless command is never
// pane-docked. Split into its own file rather than growing dock-pane.ts
// further - that file is already past its usual split bar.

import { html, type TemplateResult } from "lit-html";
import * as ipc from "../../shared/ipc";
import type { Command } from "../../types/ipc.generated";
import { undock } from "./dock-pane-poll";
import { act } from "./state";

export function refusedView(projectId: string, cmd: Command): TemplateResult {
  const stopTrying = cmd.dock_headless
    ? { title: "Stop trying to run it headless", run: () => void act(ipc.setCommandHeadless(projectId, cmd.id, false)) }
    : { title: "Stop trying to dock it", run: () => undock(`${projectId}:${cmd.id}`) };
  return html`
    <div class="dockpane-body dockpane-refused">
      <i class="ph ph-eye-slash"></i>
      <p>This app won't stay docked, so it is still on the desktop.</p>
      <button class="abtn" title=${stopTrying.title} @click=${stopTrying.run}>
        <i class="ph ph-app-window"></i> Show window normally
      </button>
    </div>
  `;
}
