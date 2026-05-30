// All dashboard modals: add-project wizard, add-command, edit-command, and the
// delete-command confirm. Holds both the render functions and the actions that
// open/confirm them. Shared view state and the re-render trigger come from
// ./state; pure helpers from ./helpers; the dropdown from ./combobox.

import { html, nothing, type TemplateResult } from "lit-html";
import { open } from "@tauri-apps/plugin-dialog";
import * as ipc from "../../shared/ipc";
import type { DetectedCommand, ProcKind } from "../../types/ipc.generated";
import {
  ui,
  draw,
  refresh,
  act,
  closeModal,
  VALIDATE_DEBOUNCE_MS,
  type Modal,
  type PickedCommand,
} from "./state";
import { basename, normPath, deriveName } from "./helpers";
import { comboBox, filterDetected } from "./combobox";

// ----- add-project wizard -----

// Find an existing project whose root matches the picked folder; return its name.
function existingProjectName(path: string): string | null {
  const want = normPath(path);
  return ui.projects.find((p) => normPath(p.root) === want)?.name ?? null;
}

async function detectInto(path: string): Promise<DetectedCommand[]> {
  try {
    return await ipc.detectCommands(path);
  } catch (e) {
    ui.error = String(e);
    return [];
  }
}

// Focus the name input once the modal has rendered, so picking a folder lands
// the cursor there immediately (folder name shows as a placeholder hint).
function focusNameField() {
  window.setTimeout(() => {
    ui.root.querySelector<HTMLInputElement>(".dialog .field-row input")?.focus();
  }, 0);
}

export async function startAddProject() {
  const picked = await open({ directory: true, multiple: false, title: "Pick a project folder" });
  if (typeof picked !== "string") return;
  const detected = await detectInto(picked);
  ui.modal = {
    t: "addProject",
    name: "",
    root: picked,
    detected,
    picked: [],
    query: "",
    highlight: -1,
    existingName: existingProjectName(picked),
  };
  ui.comboOpen = false;
  draw();
  focusNameField();
}

async function repickFolder() {
  if (ui.modal?.t !== "addProject") return;
  const picked = await open({ directory: true, multiple: false, title: "Pick a project folder" });
  if (typeof picked !== "string") return;
  const detected = await detectInto(picked);
  ui.modal = {
    t: "addProject",
    name: ui.modal.name.trim(),
    root: picked,
    detected,
    picked: [],
    query: "",
    highlight: -1,
    existingName: existingProjectName(picked),
  };
  ui.comboOpen = false;
  draw();
  focusNameField();
}

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
    kind: "generic",
    useDynamicPort: true,
    query: "",
    highlight: -1,
    check: null,
  };
  ui.comboOpen = false;
  draw();
}

async function confirmAddProject() {
  if (ui.modal?.t !== "addProject") return;
  const m = ui.modal;
  if (!m.root.trim()) {
    ui.error = "a folder is required";
    draw();
    return;
  }
  if (m.picked.length === 0) {
    ui.error = "add at least one command";
    draw();
    return;
  }
  // Empty name falls back to the folder name (shown as the input's placeholder).
  const name = m.name.trim() || basename(m.root);
  try {
    const project = await ipc.addProject(name, m.root);
    for (const p of m.picked) {
      await ipc.addCommand(project.id, p.name, p.cmd, p.kind, false, false);
    }
    ui.error = null;
    ui.modal = null;
  } catch (e) {
    ui.error = String(e);
  }
  await refresh();
}

async function confirmAddCommand() {
  if (ui.modal?.t !== "addCommand") return;
  const m = ui.modal;
  const name = m.name.trim() || deriveName(m.cmd);
  try {
    await ipc.addCommand(m.projectId, name, m.cmd, m.kind, false, m.useDynamicPort);
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
    await ipc.updateCommand(m.projectId, m.commandId, name, cmd, m.kind, m.autostart, m.useDynamicPort);
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

// Validate a free-text wizard command and stamp `ok` on the matching chip.
// Stale-guarded by modal identity + cmd presence; non-blocking.
function validatePickedFreeText(m: Extract<Modal, { t: "addProject" }>, cmd: string) {
  void ipc
    .validateCommand(m.root, cmd)
    .then((res) => {
      if (ui.modal !== m) return;
      const chip = m.picked.find((p) => p.cmd === cmd);
      if (!chip) return; // removed before resolve
      chip.ok = res.ok;
      draw();
    })
    .catch(() => {
      /* advisory only */
    });
}

// ----- render -----

function addProjectModal(m: Extract<Modal, { t: "addProject" }>): TemplateResult {
  const q = m.query.trim();
  const pickedCmds = new Set(m.picked.map((p) => p.cmd));
  const available = m.detected.filter((d) => !pickedCmds.has(d.cmd));
  const suggestions = filterDetected(available, m.query);
  // Free-text row when query is non-empty, not an exact existing suggestion cmd, and not already picked.
  const exactMatch = m.detected.some((d) => d.cmd === q) || pickedCmds.has(q);
  const showFreeText = q.length > 0 && !exactMatch;

  const addPicked = (p: PickedCommand) => {
    if (m.picked.some((x) => x.cmd === p.cmd)) return; // dedupe by cmd
    m.picked.push(p);
    m.query = "";
    m.highlight = -1;
    draw();
  };

  return html`
    <div class="overlay">
      <div class="dialog">
        <h3>Add project</h3>

        <div class="field-row">
          <label>Name</label>
          <input
            placeholder=${basename(m.root)}
            .value=${m.name}
            @input=${(e: Event) => (m.name = (e.target as HTMLInputElement).value)}
          />
        </div>
        <div class="field-row">
          <label>Folder</label>
          <span class="folder" title=${m.root}>${m.root}</span>
          <button class="ghost" tabindex="-1" @click=${() => void repickFolder()}>
            <i class="ph ph-folder-open"></i> Pick again
          </button>
        </div>

        ${m.existingName
          ? html`<p class="muted note">
              This folder is already "${m.existingName}" — new commands merge into it.
            </p>`
          : nothing}

        <div class="detect-head">
          <span>Commands</span>
        </div>

        ${m.picked.length > 0
          ? html`<div class="chips">
              ${m.picked.map(
                (p) => html`
                  <span class="chip">
                    <code>${p.cmd}</code>
                    ${p.ok === false
                      ? html`<i
                          class="ph ph-warning cmd-warn"
                          title="may not be a real command"
                        ></i>`
                      : nothing}
                    <button
                      title="Remove"
                      tabindex="-1"
                      @click=${() => {
                        m.picked = m.picked.filter((x) => x.cmd !== p.cmd);
                        draw();
                      }}
                    >
                      <i class="ph ph-x"></i>
                    </button>
                  </span>
                `,
              )}
            </div>`
          : nothing}

        ${comboBox({
          query: m.query,
          highlight: m.highlight,
          suggestions,
          showFreeText,
          placeholder: "Search detected commands or type your own…",
          onQuery: (val) => {
            m.query = val;
            m.highlight = 0; // pre-highlight the top row so Enter takes it
            draw();
          },
          onHighlight: (i) => {
            m.highlight = i;
            draw();
          },
          onSelect: (d) => addPicked({ name: d.name, cmd: d.cmd, kind: d.kind, ok: true }),
          onFreeText: () => {
            const cmd = m.query.trim();
            if (!cmd) return;
            addPicked({ name: deriveName(cmd), cmd, kind: "generic" });
            validatePickedFreeText(m, cmd);
          },
        })}

        <div class="dialog-actions">
          <button @click=${closeModal}>Cancel</button>
          <button class="primary" @click=${() => void confirmAddProject()}>Add</button>
        </div>
      </div>
    </div>
  `;
}

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
              m.kind = d.kind;
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
        <div class="field-row">
          <label>Kind</label>
          <select
            .value=${m.kind}
            @change=${(e: Event) => (m.kind = (e.target as HTMLSelectElement).value as ProcKind)}
          >
            <option value="generic">generic</option>
            <option value="flutter">flutter</option>
          </select>
        </div>
        <label class="detect-row">
          <input
            type="checkbox"
            .checked=${m.useDynamicPort}
            @change=${(e: Event) => (m.useDynamicPort = (e.target as HTMLInputElement).checked)}
          />
          <span>Assign a dynamic port</span>
        </label>
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
        <div class="field-row">
          <label>Kind</label>
          <select
            .value=${m.kind}
            @change=${(e: Event) => (m.kind = (e.target as HTMLSelectElement).value as ProcKind)}
          >
            <option value="generic">generic</option>
            <option value="flutter">flutter</option>
          </select>
        </div>
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
  }
}
