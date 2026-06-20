// Kebab popover menus for the dashboard list. cmdMenu/moreMenu render only the
// trigger button; the actual popover floats via portalMenu(), rendered at the
// root of draw() as position:fixed so it always clears every stacking context.

import { html, nothing, type TemplateResult } from "lit-html";
import { styleMap } from "lit-html/directives/style-map.js";
import type { Project } from "../../types/ipc.generated";
import * as ipc from "../../shared/ipc";
import { ui, act, draw } from "./state";
import { startAddCommand } from "./modals";

function openInBrowser(port: number, flutter: boolean) {
  void ipc.openPortUrl(`http://localhost:${port}`, flutter);
}

function copyPortUrl(port: number) {
  void navigator.clipboard?.writeText(`http://localhost:${port}`);
}

// Store the anchor rect for the portal from a button click event.
function setButtonAnchor(e: Event, menuHeight = 200) {
  const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
  ui.menuAnchor = {
    top: rect.top,
    bottom: rect.bottom,
    right: rect.right,
    flipUp: rect.bottom + menuHeight > window.innerHeight,
  };
}

// Store the anchor from a contextmenu (mouse) event. Menu opens at the cursor.
export function setMouseAnchor(e: MouseEvent, menuHeight = 200) {
  ui.menuAnchor = {
    top: e.clientY,
    bottom: e.clientY,
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
  }

  if (content === nothing) return nothing;

  const style = styleMap({
    position: "fixed",
    zIndex: "9999",
    right: `${window.innerWidth - anchor.right}px`,
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
  `;
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
