// Kebab popover menus for the dashboard list. cmdMenu/moreMenu render only the
// trigger button; the actual popover floats via portalMenu(), rendered at the
// root of draw() as position:fixed so it always clears every stacking context.

import { html, nothing, type TemplateResult } from "lit-html";
import { styleMap } from "lit-html/directives/style-map.js";
import type { Group, Project } from "../../types/ipc.generated";
import * as ipc from "../../shared/ipc";
import { ui, act, draw } from "./state";
import { startAddCommand } from "./modals";
import { startAddProject } from "./add-project";

function openInBrowser(port: number, flutter: boolean) {
  void ipc.openPortUrl(`http://localhost:${port}`, flutter);
}

function copyPortUrl(port: number) {
  void navigator.clipboard?.writeText(`http://localhost:${port}`);
}

// Store the anchor rect for the portal from a button click event.
export function setButtonAnchor(e: Event, menuHeight = 200) {
  const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
  ui.menuAnchor = {
    top: rect.top,
    bottom: rect.bottom,
    left: rect.left,
    right: rect.right,
    flipUp: rect.bottom + menuHeight > window.innerHeight,
  };
}

// Store the anchor from a contextmenu (mouse) event. Menu opens at the cursor.
export function setMouseAnchor(e: MouseEvent, menuHeight = 200) {
  ui.menuAnchor = {
    top: e.clientY,
    bottom: e.clientY,
    left: e.clientX,
    right: e.clientX,
    flipUp: e.clientY + menuHeight > window.innerHeight,
  };
}

// Per-command kebab button only. The popover is rendered by portalMenu().
export function cmdMenu(
  _project: Project,
  _cmd: Project["commands"][number],
  id: string,
  _status: string,
): TemplateResult {
  const open = ui.openCmdMenuFor === id;
  return html`
    <div class="cmd-more">
      <button
        class=${open ? "active" : ""}
        title="More options"
        @click=${(e: Event) => {
          e.stopPropagation();
          if (open) {
            ui.openCmdMenuFor = null;
            ui.menuAnchor = null;
          } else {
            setButtonAnchor(e, 200);
            ui.openCmdMenuFor = id;
          }
          draw();
        }}
      >
        <i class="ph ph-dots-three-vertical"></i>
      </button>
    </div>
  `;
}

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

// The floating menu portal. Call from draw() at root level. Renders position:fixed
// at the stored anchor so the menu is always on top of every stacking context.
export function portalMenu(): TemplateResult | typeof nothing {
  const anchor = ui.menuAnchor;
  if (!anchor) return nothing;

  let content: TemplateResult | typeof nothing = nothing;

  if (ui.openMenuFor !== null) {
    const project = ui.projects.find((p) => p.id === ui.openMenuFor);
    if (project) content = projMenuContent(project);
  } else if (ui.openCmdMenuFor !== null) {
    outer: for (const project of ui.projects) {
      for (const cmd of project.commands) {
        const id = `${project.id}:${cmd.id}`;
        if (id === ui.openCmdMenuFor) {
          const status = ui.statusById[id]?.status ?? "stopped";
          content = cmdMenuContent(project, cmd, id, status);
          break outer;
        }
      }
    }
  } else if (ui.openGroupMenuFor !== null) {
    const group = ui.groups.find((g) => g.id === ui.openGroupMenuFor);
    if (group) content = groupMenuContent(group);
  } else if (ui.openMoveToGroupFor !== null) {
    content = moveToGroupContent(ui.openMoveToGroupFor);
  } else if (ui.openEmptyMenu) {
    content = emptyMenuContent();
  }

  if (content === nothing) return nothing;

  const menuWidth = 200;
  const openLeft = anchor.right >= menuWidth;
  const style = styleMap({
    position: "fixed",
    zIndex: "9999",
    right: openLeft ? `${window.innerWidth - anchor.right}px` : undefined,
    left: openLeft ? undefined : `${anchor.left}px`,
    top: anchor.flipUp ? undefined : `${anchor.bottom + 4}px`,
    bottom: anchor.flipUp ? `${window.innerHeight - anchor.top + 4}px` : undefined,
  });

  return html`
    <div class="more-menu" style=${style} @click=${(e: Event) => e.stopPropagation()}>
      ${content}
    </div>
  `;
}

function projMenuContent(project: Project): TemplateResult {
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

function groupMenuContent(group: Group): TemplateResult {
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

function moveToGroupContent(projectId: string): TemplateResult {
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

function emptyMenuContent(): TemplateResult {
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

// "New project in group": stash the target group so confirmAddProject (or
// closeModal, on cancel) can assign/clear it once the wizard resolves.
function startAddProjectInGroup(groupId: string) {
  ui.pendingGroupId = groupId;
  void startAddProject();
}

function cmdMenuContent(
  project: Project,
  cmd: Project["commands"][number],
  id: string,
  status: string,
): TemplateResult {
  const live = status === "running" || status === "starting";
  const port = ui.statusById[id]?.port;
  const isFlutter = cmd.kind === "flutter";
  const close = () => {
    ui.openCmdMenuFor = null;
    ui.menuAnchor = null;
  };
  return html`
    ${live
      ? html`
          ${port != null
            ? html`
                <button
                  class="accent"
                  @click=${() => {
                    close();
                    openInBrowser(port, isFlutter);
                  }}
                >
                  <i class="ph ph-globe-simple"></i> Open :${port} in browser
                </button>
                <button @click=${() => { close(); copyPortUrl(port); }}>
                  <i class="ph ph-copy"></i> Copy URL
                </button>
                <div class="menu-div"></div>
              `
            : nothing}
          <button
            @click=${() => {
              close();
              if (ui.openLogsFor === id) ui.logText = "";
              void act(ipc.restartProc(id));
            }}
          >
            <i class="ph ph-arrow-clockwise"></i> Restart
          </button>
          <button
            @click=${() => {
              close();
              if (ui.openLogsFor === id) ui.logText = "";
              void act(ipc.stopProc(id));
            }}
          >
            <i class="ph ph-stop"></i> Stop
          </button>
        `
      : html`
          <button
            @click=${() => {
              close();
              ui.modal = {
                t: "editCommand",
                projectId: project.id,
                commandId: cmd.id,
                root: project.root,
                name: cmd.name,
                cmd: cmd.cmd,
                autostart: cmd.autostart,
                useDynamicPort: cmd.use_dynamic_port,
                env: cmd.env,
                check: null,
              };
              ui.comboOpen = false;
              draw();
            }}
          >
            <i class="ph ph-pencil-simple"></i> Edit command
          </button>
          <button
            @click=${() => {
              close();
              ui.modal = {
                t: "confirmDeleteCommand",
                projectId: project.id,
                commandId: cmd.id,
                cmdName: cmd.name,
                lastOne: project.commands.length === 1,
              };
              draw();
            }}
          >
            <i class="ph ph-trash"></i> Remove command
          </button>
        `}
  `;
}
