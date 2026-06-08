// Command modals: add-command, edit-command, and the delete-command confirm,
// plus the actions that open/confirm them and the shared cmd validation. The
// add-project wizard lives in ./add-project (this module imports its render fn).
// Shared view state and the re-render trigger come from ./state; pure helpers
// from ./helpers; the dropdown from ./combobox.

import "./modals.css";
import { html, nothing, type TemplateResult } from "lit-html";
import * as ipc from "../../shared/ipc";
import {
  ui,
  draw,
  refresh,
  act,
  closeModal,
  VALIDATE_DEBOUNCE_MS,
  type Modal,
} from "./state";
import { deriveName } from "./helpers";
import { comboBox, filterDetected } from "./combobox";
import { addProjectModal, detectInto } from "./add-project";

// Open the add-command modal for a project, pre-loading detected commands.
export async function startAddCommand(projectId: string, root: string) {
  const detected = await detectInto(root);
  ui.modal = {
    t: "addCommand",
    projectId,
    root,
    detected,
    name: "",
    cmd: "",
    useDynamicPort: true,
    env: "",
    query: "",
    highlight: -1,
    check: null,
  };
  ui.comboOpen = false;
  draw();
}

async function confirmAddCommand() {
  if (ui.modal?.t !== "addCommand") return;
  const m = ui.modal;
  const name = m.name.trim() || deriveName(m.cmd);
  try {
    await ipc.addCommand(m.projectId, name, m.cmd, false, m.useDynamicPort, m.env);
    ui.error = null;
    ui.modal = null;
  } catch (e) {
    ui.error = String(e);
  }
  await refresh();
}

async function confirmEditCommand() {
  if (ui.modal?.t !== "editCommand") return;
  const m = ui.modal;
  const cmd = m.cmd.trim();
  if (!cmd) {
    ui.error = "command is required";
    draw();
    return;
  }
  const name = m.name.trim() || deriveName(cmd);
  try {
    await ipc.updateCommand(m.projectId, m.commandId, name, cmd, m.autostart, m.useDynamicPort, m.env);
    ui.error = null;
    ui.modal = null;
  } catch (e) {
    ui.error = String(e);
  }
  await refresh();
}

// The two modals that carry a free-text `cmd` to validate (add + edit).
type CmdModal = Extract<Modal, { t: "addCommand" } | { t: "editCommand" }>;
function cmdModal(): CmdModal | null {
  return ui.modal && (ui.modal.t === "addCommand" || ui.modal.t === "editCommand")
    ? ui.modal
    : null;
}

// Optional per-command env overrides, one KEY=VALUE per line. Values may
// reference existing vars via ${NAME} / %NAME% (so PATH=...;%PATH% prepends).
// Lets a command reach a toolchain the inherited env can't (e.g. node past the
// nvm4w symlink) without a hand-rolled wrapper script.
function envField(m: CmdModal): TemplateResult {
  return html`
    <div class="field-row env-row">
      <label>Env</label>
      <textarea
        class="env-input"
        rows="2"
        spellcheck="false"
        placeholder="optional — KEY=VALUE per line, e.g. PATH=C:\\node\\dir;%PATH%"
        .value=${m.env}
        @input=${(e: Event) => (m.env = (e.target as HTMLTextAreaElement).value)}
      ></textarea>
    </div>
  `;
}

// Debounced advisory check for a cmd-bearing modal's `cmd`. Stale-guarded: only
// applies if the same modal is still open and its cmd still matches what was
// validated. Never blocks; failures are silently ignored.
function scheduleValidate() {
  if (ui.validateTimer !== undefined) window.clearTimeout(ui.validateTimer);
  ui.validateTimer = window.setTimeout(() => {
    ui.validateTimer = undefined;
    const m = cmdModal();
    if (!m) return;
    const cmd = m.cmd.trim();
    if (!cmd) {
      m.check = null;
      draw();
      return;
    }
    const root = m.root;
    void ipc
      .validateCommand(root, cmd)
      .then((res) => {
        // Guard against stale results from a fast typer / reopened modal.
        if (ui.modal !== m || m.cmd.trim() !== cmd || m.root !== root) return;
        m.check = res;
        draw();
      })
      .catch(() => {
        /* advisory only: ignore resolver errors */
      });
  }, VALIDATE_DEBOUNCE_MS);
}

// ----- render -----

function addCommandModal(m: Extract<Modal, { t: "addCommand" }>): TemplateResult {
  const q = m.query.trim();
  const suggestions = filterDetected(m.detected, m.query);
  const exactMatch = m.detected.some((d) => d.cmd === q);
  const showFreeText = q.length > 0 && !exactMatch;

  return html`
    <div class="overlay">
      <div class="dialog">
        <h3>Add command</h3>
        ${comboBox({
            query: m.query,
            highlight: m.highlight,
            suggestions,
            showFreeText,
            placeholder: "npm run dev:up",
            onQuery: (val) => {
              m.query = val;
              m.cmd = val; // free-text: cmd tracks the typed text
              m.highlight = 0; // pre-highlight the top row so Enter takes it
              m.check = null; // clear stale warning until the debounce resolves
              scheduleValidate();
              draw();
            },
            onHighlight: (i) => {
              m.highlight = i;
              draw();
            },
            onSelect: (d) => {
              m.cmd = d.cmd;
              m.name = d.name;
              m.query = d.cmd;
              m.highlight = -1;
              m.check = null; // a detected command is inherently valid
              if (ui.validateTimer !== undefined) {
                window.clearTimeout(ui.validateTimer);
                ui.validateTimer = undefined;
              }
              ui.comboOpen = false;
              draw();
            },
            onFreeText: () => {
              const cmd = m.query.trim();
              m.cmd = cmd;
              if (!m.name.trim()) m.name = deriveName(cmd);
              m.highlight = -1;
              ui.comboOpen = false;
              scheduleValidate();
              draw();
            },
          })}
        ${m.check && !m.check.ok
          ? html`<div class="cmd-warn">
              <i class="ph ph-warning"></i>
              <span title=${m.check.reason}>${m.check.reason}</span>
            </div>`
          : nothing}
        <label class="detect-row">
          <input
            type="checkbox"
            .checked=${m.useDynamicPort}
            @change=${(e: Event) => (m.useDynamicPort = (e.target as HTMLInputElement).checked)}
          />
          <span>Assign a dynamic port</span>
        </label>
        ${envField(m)}
        <div class="dialog-actions">
          <button @click=${closeModal}>Cancel</button>
          <button class="primary" @click=${() => void confirmAddCommand()}>Add</button>
        </div>
      </div>
    </div>
  `;
}

function editCommandModal(m: Extract<Modal, { t: "editCommand" }>): TemplateResult {
  return html`
    <div class="overlay">
      <div class="dialog">
        <h3>Edit command</h3>
        <div class="field-row">
          <label>Name</label>
          <input
            placeholder=${deriveName(m.cmd)}
            .value=${m.name}
            @input=${(e: Event) => (m.name = (e.target as HTMLInputElement).value)}
          />
        </div>
        <div class="field-row">
          <label>Command</label>
          <input
            placeholder="npm run dev:up"
            .value=${m.cmd}
            @input=${(e: Event) => {
              m.cmd = (e.target as HTMLInputElement).value;
              m.check = null; // clear stale warning until the debounce resolves
              scheduleValidate();
              draw();
            }}
          />
        </div>
        ${m.check && !m.check.ok
          ? html`<div class="cmd-warn">
              <i class="ph ph-warning"></i>
              <span title=${m.check.reason}>${m.check.reason}</span>
            </div>`
          : nothing}
        <label class="detect-row">
          <input
            type="checkbox"
            .checked=${m.useDynamicPort}
            @change=${(e: Event) => (m.useDynamicPort = (e.target as HTMLInputElement).checked)}
          />
          <span>Assign a dynamic port</span>
        </label>
        <label class="detect-row">
          <input
            type="checkbox"
            .checked=${m.autostart}
            @change=${(e: Event) => (m.autostart = (e.target as HTMLInputElement).checked)}
          />
          <span>Start automatically when the supervisor launches</span>
        </label>
        ${envField(m)}
        <p class="muted note">Saving relaunches the command if it's running.</p>
        <div class="dialog-actions">
          <button @click=${closeModal}>Cancel</button>
          <button class="primary" @click=${() => void confirmEditCommand()}>Save</button>
        </div>
      </div>
    </div>
  `;
}

function confirmDeleteCommandModal(
  m: Extract<Modal, { t: "confirmDeleteCommand" }>,
): TemplateResult {
  return html`
    <div class="overlay">
      <div class="dialog">
        <h3>Delete command</h3>
        <p class="muted note">
          Delete <code>${m.cmdName}</code>?
          ${m.lastOne
            ? html`<br />It's the only command, so the project will be removed too.`
            : nothing}
        </p>
        <div class="dialog-actions">
          <button @click=${closeModal}>Cancel</button>
          <button
            class="danger"
            @click=${async () => {
              const { projectId, commandId } = m;
              ui.modal = null;
              await act(ipc.removeCommand(projectId, commandId));
            }}
          >
            Delete
          </button>
        </div>
      </div>
    </div>
  `;
}

function confirmStopAllModal(
  m: Extract<Modal, { t: "confirmStopAll" }>,
): TemplateResult {
  return html`
    <div class="overlay">
      <div class="dialog">
        <h3>Stop all processes</h3>
        <p class="muted note">
          Stop all <code>${m.count}</code> running processes? The app stays open.
        </p>
        <div class="dialog-actions">
          <button @click=${closeModal}>Cancel</button>
          <button
            class="danger"
            @click=${async () => {
              ui.modal = null;
              await act(ipc.stopAllProcs());
            }}
          >
            Stop all
          </button>
        </div>
      </div>
    </div>
  `;
}

export function modalView(): TemplateResult | typeof nothing {
  const m = ui.modal;
  if (!m) return nothing;
  switch (m.t) {
    case "addProject":
      return addProjectModal(m);
    case "addCommand":
      return addCommandModal(m);
    case "editCommand":
      return editCommandModal(m);
    case "confirmDeleteCommand":
      return confirmDeleteCommandModal(m);
    case "confirmStopAll":
      return confirmStopAllModal(m);
  }
}
