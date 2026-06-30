// The add-project wizard: folder picking, command detection, and the
// chip/combobox UI for choosing which detected (or free-text) commands to add.
// Split out of modals.ts as the heaviest, most self-contained modal. Shared view
// state + the re-render trigger come from ./state; pure helpers from ./helpers;
// the dropdown from ./combobox. Depends only on those (never on ./modals), so
// the import graph stays acyclic (modals.ts -> add-project.ts, not the reverse).

import { html, nothing, type TemplateResult } from "lit-html";
import { open } from "@tauri-apps/plugin-dialog";
import * as ipc from "../../shared/ipc";
import type { DetectedCommand } from "../../types/ipc.generated";
import { ui, draw, refresh, closeModal, type Modal, type PickedCommand } from "./state";
import { normPath, deriveName, smartProjectName } from "./helpers";
import { comboBox, filterDetected } from "./combobox";

// Find an existing project whose root matches the picked folder; return its name.
function existingProjectName(path: string): string | null {
  const want = normPath(path);
  return ui.projects.find((p) => normPath(p.root) === want)?.name ?? null;
}

// Detect runnable commands in a folder. Shared with the add-command modal, so it
// lives here (the wizard owns detection) and modals.ts imports it.
export async function detectInto(path: string): Promise<DetectedCommand[]> {
  try {
    return await ipc.detectCommands(path);
  } catch (e) {
    ui.error = String(e);
    return [];
  }
}

// Focus the command combobox once the modal has rendered: picking a folder lands
// the cursor on the field you actually fill (Name is optional and defaults to the
// folder name, shown as its placeholder).
function focusCommandField() {
  window.setTimeout(() => {
    ui.root.querySelector<HTMLInputElement>(".dialog .combo-input")?.focus();
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
  focusCommandField();
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
  focusCommandField();
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
  // Empty name falls back to the smart default (shown as the input's placeholder).
  const name = m.name.trim() || smartProjectName(m.root);
  try {
    const project = await ipc.addProject(name, m.root);
    for (const p of m.picked) {
      await ipc.addCommand(project.id, p.name, p.cmd, false, false);
    }
    if (ui.pendingGroupId) {
      await ipc.setProjectGroup(project.id, ui.pendingGroupId);
      ui.pendingGroupId = null;
    }
    ui.error = null;
    ui.modal = null;
  } catch (e) {
    ui.error = String(e);
  }
  await refresh();
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

export function addProjectModal(m: Extract<Modal, { t: "addProject" }>): TemplateResult {
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
            placeholder=${smartProjectName(m.root)}
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
          onSelect: (d) => addPicked({ name: d.name, cmd: d.cmd, ok: true }),
          onFreeText: () => {
            const cmd = m.query.trim();
            if (!cmd) return;
            addPicked({ name: deriveName(cmd), cmd });
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
