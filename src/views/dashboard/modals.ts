// Command modals (add-command, edit-command) live in ./command-modals; this
// module wires them into the modalView switch and keeps the delete-command
// confirm + rename-project modal. The add-project wizard lives in ./add-project
// (this module imports its render fn); the preset modals live in
// ./preset-modals (this module wires them into the modalView switch); the
// port/role/env field builders live in ./modal-fields. Shared view state and
// the re-render trigger come from ./state; pure helpers from ./helpers.

import "./modals.css";
import { html, nothing, type TemplateResult } from "lit-html";
import * as ipc from "../../shared/ipc";
import { ui, refresh, act, closeModal, type Modal } from "./state";
import { requireField } from "./helpers";
import { addProjectModal } from "./add-project";
import { addPresetModal, editPresetModal } from "./preset-modals";
import { combineCommandsModal } from "./combine-modal";
import { addCommandModal, editCommandModal } from "./command-modals";

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

async function confirmRenameProject() {
  if (ui.modal?.t !== "renameProject") return;
  const m = ui.modal;
  const name = requireField(m.name, "project name is required");
  if (!name) return;
  try {
    await ipc.renameProject(m.projectId, name);
    ui.error = null;
    ui.modal = null;
  } catch (e) {
    ui.error = String(e);
  }
  await refresh();
}

function renameProjectModal(
  m: Extract<Modal, { t: "renameProject" }>,
): TemplateResult {
  return html`
    <div class="overlay">
      <div class="dialog">
        <h3>Rename project</h3>
        <div class="field-row">
          <label>Name</label>
          <input
            autofocus
            .value=${m.name}
            @input=${(e: Event) => (m.name = (e.target as HTMLInputElement).value)}
            @keydown=${(e: KeyboardEvent) => {
              if (e.key === "Enter") void confirmRenameProject();
            }}
          />
        </div>
        <div class="dialog-actions">
          <button @click=${closeModal}>Cancel</button>
          <button class="primary" @click=${() => void confirmRenameProject()}>Save</button>
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
    case "combineCommands":
      return combineCommandsModal(m);
    case "renameProject":
      return renameProjectModal(m);
    case "addPreset":
      return addPresetModal(m);
    case "editPreset":
      return editPresetModal(m);
  }
}
