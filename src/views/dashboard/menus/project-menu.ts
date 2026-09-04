// Per-project kebab menu: trigger button + popover content, rendered via
// portalMenu() in ../menus.ts.

import { html, type TemplateResult } from "lit-html";
import type { Project } from "../../../types/ipc.generated";
import * as ipc from "../../../shared/ipc";
import { ui, draw } from "../state";
import { startAddCommand } from "../modals";
import { startAddProject } from "../add-project";
import { setButtonAnchor } from "../menus";

// Per-project kebab button only. The popover is rendered by portalMenu().
export function moreMenu(project: Project): TemplateResult {
  const open = ui.openMenuFor === project.id;
  return html`
    <div class="proj-more ${open ? "menu-open" : ""}">
      <button
        class="abtn ${open ? "active" : ""}"
        title="More options"
        @click=${(e: Event) => {
          e.stopPropagation();
          if (open) {
            ui.openMenuFor = null;
            ui.menuAnchor = null;
          } else {
            setButtonAnchor(e, 140);
            ui.openMenuFor = project.id;
          }
          draw();
        }}
      >
        <i class="ph ph-dots-three-vertical"></i>
      </button>
    </div>
  `;
}

export function projMenuContent(project: Project): TemplateResult {
  const close = () => {
    ui.openMenuFor = null;
    ui.menuAnchor = null;
  };
  return html`
    <button
      @click=${() => {
        close();
        void startAddCommand(project.id, project.root);
      }}
    >
      <i class="ph ph-plus"></i> Add command
    </button>
    <button
      @click=${() => {
        close();
        ui.modal = { t: "renameProject", projectId: project.id, name: project.name };
        draw();
      }}
    >
      <i class="ph ph-pencil-simple"></i> Rename project
    </button>
    <button
      @click=${() => {
        close();
        void ipc.openInExplorer(project.root);
        draw();
      }}
    >
      <i class="ph ph-folder-open"></i> Open in file explorer
    </button>
    <button
      @click=${() => {
        ui.openMenuFor = null;
        ui.openMoveToGroupFor = project.id;
        draw();
      }}
    >
      <i class="ph ph-rows"></i> Move to group
    </button>
  `;
}

// "New project in group": stash the target group so confirmAddProject (or
// closeModal, on cancel) can assign/clear it once the wizard resolves.
export function startAddProjectInGroup(groupId: string) {
  ui.pendingGroupId = groupId;
  void startAddProject();
}
