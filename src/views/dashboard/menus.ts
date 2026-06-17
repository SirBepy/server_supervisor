// Kebab popover menus for the dashboard list. cmdMenu is the per-command kebab
// (Open in browser / Copy URL / Restart / Stop, or Edit / Remove); moreMenu is
// the per-project kebab (Add command / Rename project / Open in file explorer).
// Split out of dashboard.ts to keep the controller focused (one concern per file).

import { html, nothing, type TemplateResult } from "lit-html";
import type { Project } from "../../types/ipc.generated";
import * as ipc from "../../shared/ipc";
import { ui, act, draw } from "./state";
import { startAddCommand } from "./modals";

// Open a running command's port in a browser (from the kebab menu). Flutter-web
// ports go to the dedicated CORS-disabled dev browser (new tab in the same
// window); everything else opens in the default browser. Always http: these are
// localhost dev servers (and the flutter reload proxy), never https.
function openInBrowser(port: number, flutter: boolean) {
  void ipc.openPortUrl(`http://localhost:${port}`, flutter);
}

// Copy a command's localhost URL to the clipboard (from the kebab menu).
function copyPortUrl(port: number) {
  void navigator.clipboard?.writeText(`http://localhost:${port}`);
}

// Per-command "more options" (kebab) menu. For a live process (running or
// starting): Open-in-browser + Copy URL for its port (the port is no longer a
// bare clickable badge - opening it lives here), then Restart / Stop. For a
// stopped or crashed process: Edit / Remove (you can't edit a live command).
export function cmdMenu(
  project: Project,
  cmd: Project["commands"][number],
  id: string,
  status: string,
): TemplateResult {
  const open = ui.openCmdMenuFor === id;
  const live = status === "running" || status === "starting";
  const port = ui.statusById[id]?.port;
  const isFlutter = cmd.kind === "flutter";
  const close = () => {
    ui.openCmdMenuFor = null;
  };
  return html`
    <div class="cmd-more">
      <button
        class=${open ? "active" : ""}
        title="More options"
        @click=${(e: Event) => {
          e.stopPropagation();
          if (!open) {
            const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
            ui.cmdMenuFlipUp = rect.bottom + 200 > window.innerHeight;
          }
          ui.openCmdMenuFor = open ? null : id;
          draw();
        }}
      >
        <i class="ph ph-dots-three-vertical"></i>
      </button>
      ${open
        ? html`
            <div class="more-menu ${ui.cmdMenuFlipUp ? "flip-up" : ""}" @click=${(e: Event) => e.stopPropagation()}>
              ${live
                ? html`
                    ${port != null
                      ? html`
                          <button class="accent" @click=${() => { close(); openInBrowser(port, isFlutter); }}>
                            <i class="ph ph-globe-simple"></i> Open :${port} in browser
                          </button>
                          <button @click=${() => { close(); copyPortUrl(port); }}>
                            <i class="ph ph-copy"></i> Copy URL
                          </button>
                          <div class="menu-div"></div>
                        `
                      : nothing}
                    <button @click=${() => { close(); void act(ipc.restartProc(id)); }}>
                      <i class="ph ph-arrow-clockwise"></i> Restart
                    </button>
                    <button @click=${() => { close(); void act(ipc.stopProc(id)); }}>
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
            </div>
          `
        : nothing}
    </div>
  `;
}

// Per-project "more options" (kebab) button + its popover menu. The kebab trigger
// lives in the row's hover swap zone (.prow-actions); this provides the button +
// anchored menu (Add command / Rename project / Open in file explorer).
export function moreMenu(project: Project): TemplateResult {
  const open = ui.openMenuFor === project.id;
  return html`
    <div class="proj-more ${open ? "menu-open" : ""}">
      <button
        class="abtn ${open ? "active" : ""}"
        title="More options"
        @click=${(e: Event) => {
          e.stopPropagation();
          if (!open) {
            const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
            ui.projMenuFlipUp = rect.bottom + 140 > window.innerHeight;
          }
          ui.openMenuFor = open ? null : project.id;
          draw();
        }}
      >
        <i class="ph ph-dots-three-vertical"></i>
      </button>
      ${open
        ? html`
            <div class="more-menu ${ui.projMenuFlipUp ? "flip-up" : ""}" @click=${(e: Event) => e.stopPropagation()}>
              <button
                @click=${() => {
                  ui.openMenuFor = null;
                  void startAddCommand(project.id, project.root);
                }}
              >
                <i class="ph ph-plus"></i> Add command
              </button>
              <button
                @click=${() => {
                  ui.openMenuFor = null;
                  ui.modal = { t: "renameProject", projectId: project.id, name: project.name };
                  draw();
                }}
              >
                <i class="ph ph-pencil-simple"></i> Rename project
              </button>
              <button
                @click=${() => {
                  ui.openMenuFor = null;
                  void ipc.openInExplorer(project.root);
                  draw();
                }}
              >
                <i class="ph ph-folder-open"></i> Open in file explorer
              </button>
            </div>
          `
        : nothing}
    </div>
  `;
}
