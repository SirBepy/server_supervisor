// Per-command kebab menu: trigger button + popover content, rendered via
// portalMenu() in ../menus.ts.

import { html, nothing, type TemplateResult } from "lit-html";
import type { CommandParam, Project } from "../../../types/ipc.generated";
import * as ipc from "../../../shared/ipc";
import { ui, act, draw } from "../state";
import { setButtonAnchor, openInBrowser, copyPortUrl } from "../menus";
import { isDockRefused } from "../dock-pane";

// The value a param would spawn with right now: the stored `last_value`'s
// value, else the first value - mirrors the backend's own fallback
// (`CommandParam.last_value` doc, `resolve_value` in param_sub.rs).
function currentValue(param: CommandParam) {
  return param.values.find((v) => v.value === param.last_value) ?? param.values[0];
}

// One row per param, e.g. "Device: Chrome". Clicking swaps the popover to
// that param's value list via ui.openParamPickerFor (the same content-swap
// state shape ui.openMoveToGroupFor uses for moveToGroupContent).
function paramPickerRow(id: string, param: CommandParam): TemplateResult {
  const current = currentValue(param);
  return html`
    <button
      @click=${(e: Event) => {
        e.stopPropagation();
        ui.openParamPickerFor = { procId: id, paramName: param.name };
        draw();
      }}
    >
      <i class="ph ph-sliders-horizontal"></i>
      ${param.label}: ${current?.label ?? "(none)"}
    </button>
  `;
}

// The swapped-in content once a param row has been clicked: a back row, then
// one row per value (current one checked). Picking the already-current value
// just closes the menu; picking a different one persists it and restarts the
// command if it's running (backend's set_command_param).
function paramValuePicker(
  project: Project,
  cmd: Project["commands"][number],
  param: CommandParam,
): TemplateResult {
  const current = currentValue(param);
  const closeAll = () => {
    ui.openCmdMenuFor = null;
    ui.openParamPickerFor = null;
    ui.menuAnchor = null;
  };
  return html`
    <button
      @click=${(e: Event) => {
        e.stopPropagation();
        ui.openParamPickerFor = null;
        draw();
      }}
    >
      <i class="ph ph-arrow-left"></i> Back
    </button>
    <div class="menu-div"></div>
    ${param.values.map(
      (v) => html`
        <button
          @click=${() => {
            const picked = v.value;
            const wasCurrent = current?.value === picked;
            closeAll();
            if (wasCurrent) {
              draw();
            } else {
              void act(ipc.setCommandParam(project.id, cmd.id, param.name, picked));
            }
          }}
        >
          ${v.value === current?.value ? html`<i class="ph ph-check"></i>` : nothing}
          ${v.label}
        </button>
      `,
    )}
  `;
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
            ui.openParamPickerFor = null;
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
  // Swapped-in picker view takes over the whole popover, same as
  // moveToGroupContent does for its own ui state field. A stale picker (its
  // param got renamed/removed while open) falls through to the normal menu.
  const picker = ui.openParamPickerFor;
  if (picker && picker.procId === id) {
    const param = cmd.params.find((p) => p.name === picker.paramName);
    if (param) return paramValuePicker(project, cmd, param);
  }
  const live = status === "running" || status === "starting";
  const port = ui.statusById[id]?.port;
  const isFlutter = cmd.kind === "flutter";
  const close = () => {
    ui.openCmdMenuFor = null;
    ui.openParamPickerFor = null;
    ui.menuAnchor = null;
  };
  // Both apply live to a running command, so they sit outside the
  // running/stopped split below.
  const liveToggles = html`
    <button
      @click=${() => {
        close();
        void act(ipc.setCommandSound(project.id, cmd.id, !cmd.play_sound));
      }}
    >
      <i class="ph ${cmd.play_sound ? "ph-speaker-slash" : "ph-speaker-high"}"></i>
      ${cmd.play_sound ? "Mute sound" : "Let sound through"}
    </button>
    <button
      @click=${() => {
        close();
        void act(ipc.setCommandHeadless(project.id, cmd.id, !cmd.dock_headless));
      }}
    >
      <i class="ph ${cmd.dock_headless ? "ph-app-window" : "ph-eye-slash"}"></i>
      ${cmd.dock_headless
        ? isDockRefused(id)
          ? "Show window normally (refused headless)"
          : "Show window normally"
        : "Run window headless"}
    </button>
  `;
  // Rendered right after liveToggles, outside the live/stopped split below,
  // so the picker shows whether the command is running or stopped (switching
  // variant mid-session - zng-app web-server to chrome for a Playwright
  // check - is the common case, not an edge one).
  const paramRows = cmd.params.length > 0 ? html`${cmd.params.map((p) => paramPickerRow(id, p))}` : nothing;
  return html`
    ${liveToggles}
    ${paramRows}
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
                // Deep-copied so editing values in the modal (then cancelling)
                // never mutates the live Command the kebab/rows still read.
                params: cmd.params.map((p) => ({ ...p, values: p.values.map((v) => ({ ...v })) })),
                paramsError: null,
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
