// Per-group kebab menu + move-to-group/empty-space popover content, rendered
// via portalMenu() in ../menus.ts.

import { html, nothing, type TemplateResult } from "lit-html";
import type { Group } from "../../../types/ipc.generated";
import * as ipc from "../../../shared/ipc";
import { ui, act, draw } from "../state";
import { startAddProject } from "../add-project";
import { setButtonAnchor } from "../menus";
import { startAddProjectInGroup } from "./project-menu";

// Per-group kebab button only. The popover is rendered by portalMenu().
export function groupMenu(group: Group): TemplateResult {
  const open = ui.openGroupMenuFor === group.id;
  return html`
    <div class="proj-more ${open ? "menu-open" : ""}">
      <button
        class="abtn ${open ? "active" : ""}"
        title="Group options"
        @click=${(e: Event) => {
          e.stopPropagation();
          if (open) {
            ui.openGroupMenuFor = null;
            ui.menuAnchor = null;
          } else {
            setButtonAnchor(e, 120);
            ui.openGroupMenuFor = group.id;
          }
          draw();
        }}
      >
        <i class="ph ph-dots-three-vertical"></i>
      </button>
    </div>
  `;
}

export function groupMenuContent(group: Group): TemplateResult {
  const close = () => {
    ui.openGroupMenuFor = null;
    ui.menuAnchor = null;
  };
  return html`
    <button
      @click=${() => {
        close();
        const name = window.prompt("Rename group:", group.name);
        if (name && name.trim() && name.trim() !== group.name) {
          void act(ipc.updateGroup(group.id, name.trim()));
        } else {
          draw();
        }
      }}
    >
      <i class="ph ph-pencil-simple"></i> Rename group
    </button>
    <button
      @click=${() => {
        close();
        void startAddProjectInGroup(group.id);
      }}
    >
      <i class="ph ph-plus"></i> New project in "${group.name}"
    </button>
    <button
      class="danger"
      @click=${() => {
        close();
        if (window.confirm(`Delete group "${group.name}"? Projects will become ungrouped.`)) {
          void act(ipc.deleteGroup(group.id));
        } else {
          draw();
        }
      }}
    >
      <i class="ph ph-trash"></i> Delete group
    </button>
  `;
}

export function moveToGroupContent(projectId: string): TemplateResult {
  const currentGroupId = ui.groups.find((g) => g.project_ids.includes(projectId))?.id ?? null;
  const close = () => {
    ui.openMoveToGroupFor = null;
    ui.menuAnchor = null;
  };
  return html`
    ${ui.groups.map(
      (g) => html`
        <button
          @click=${() => {
            close();
            const newGroupId = currentGroupId === g.id ? null : g.id;
            void act(ipc.setProjectGroup(projectId, newGroupId));
          }}
        >
          ${currentGroupId === g.id ? html`<i class="ph ph-check"></i>` : nothing}
          ${g.name}
        </button>
      `,
    )}
    <button
      @click=${() => {
        close();
        const name = window.prompt("New group name:");
        if (name?.trim()) {
          void ipc.createGroup(name.trim()).then((g) =>
            act(ipc.setProjectGroup(projectId, g.id)),
          );
        } else {
          draw();
        }
      }}
    >
      <i class="ph ph-plus"></i> New group...
    </button>
    ${currentGroupId !== null
      ? html`
          <button
            @click=${() => {
              close();
              void act(ipc.setProjectGroup(projectId, null));
            }}
          >
            <i class="ph ph-x"></i> Ungroup
          </button>
        `
      : nothing}
  `;
}

export function emptyMenuContent(): TemplateResult {
  const close = () => {
    ui.openEmptyMenu = false;
    ui.menuAnchor = null;
  };
  return html`
    <button
      @click=${() => {
        close();
        const name = window.prompt("Group name:");
        if (name?.trim()) {
          void act(ipc.createGroup(name.trim()));
        } else {
          draw();
        }
      }}
    >
      <i class="ph ph-rows"></i> New group
    </button>
    <button
      @click=${() => {
        close();
        void startAddProject();
      }}
    >
      <i class="ph ph-folder-plus"></i> New project (ungrouped)
    </button>
  `;
}
