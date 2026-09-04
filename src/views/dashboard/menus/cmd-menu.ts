// Per-command kebab menu: trigger button + popover content, rendered via
// portalMenu() in ../menus.ts.

import { html, nothing, type TemplateResult } from "lit-html";
import type { Project } from "../../../types/ipc.generated";
import * as ipc from "../../../shared/ipc";
import { ui, act, draw } from "../state";
import { setButtonAnchor, openInBrowser, copyPortUrl } from "../menus";

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

export function cmdMenuContent(
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
      ? // Restart/Stop live as inline buttons on the Project screen row too,
        // but right-clicking a row (this menu) needs them as well - a
        // right-click "Stop" was the whole point of adding this branch.
        html`
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
              `
            : nothing}
          <button
            @click=${() => {
              close();
              void act(ipc.restartProc(id));
            }}
          >
            <i class="ph ph-arrow-clockwise"></i> Restart
          </button>
          <button
            @click=${() => {
              close();
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
                port: cmd.fixed_port != null ? String(cmd.fixed_port) : "",
                portError: null,
                env: cmd.env,
                role: cmd.role,
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
