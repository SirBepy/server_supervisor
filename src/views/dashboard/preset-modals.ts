// Reverse-proxy hub: preset add/edit modals, plus the actions that
// open/confirm them and the shared base-URL validation. Shared view state
// and the re-render trigger come from ./state; pure helpers from ./helpers.

import { html, nothing, type TemplateResult } from "lit-html";
import * as ipc from "../../shared/ipc";
import { ui, draw, refresh, closeModal, type Modal } from "./state";
import { fieldError, requireField } from "./helpers";
import type { UpstreamPreset } from "../../types/ipc.generated";

// Open the add-preset modal for a project.
export function startAddPreset(projectId: string) {
  ui.modal = { t: "addPreset", projectId, name: "", baseUrl: "", danger: false, urlError: null };
  draw();
}

// Open the edit-preset modal, pre-filled from the existing preset. `wasActive`
// records whether this preset is currently serving traffic, so confirmEditPreset
// can re-point active_preset at the replacement id it creates (there is no
// backend `update_preset` - see the `editPreset` Modal variant's doc comment
// in state.ts for why this is a remove-then-add composite instead).
export function startEditPreset(projectId: string, preset: UpstreamPreset, wasActive: boolean) {
  ui.modal = {
    t: "editPreset",
    projectId,
    presetId: preset.id,
    wasActive,
    name: preset.name,
    baseUrl: preset.base_url,
    danger: preset.danger,
    urlError: null,
  };
  draw();
}

// The two modals that carry a free-text `baseUrl` to validate (add + edit preset).
type PresetModal = Extract<Modal, { t: "addPreset" } | { t: "editPreset" }>;

// Parse a preset modal's raw base-URL text. Must be non-empty and parse as a
// URL (the hub forwards to it verbatim); anything else is rejected locally via
// `urlError`, matching the inline `portError` pattern on the command modals.
function parseBaseUrlField(m: PresetModal): { baseUrl: string; ok: true } | { ok: false } {
  const text = m.baseUrl.trim();
  if (!text) {
    m.urlError = "base URL is required";
    return { ok: false };
  }
  try {
    new URL(text);
  } catch {
    m.urlError = "must be a valid URL, e.g. http://localhost:9000";
    return { ok: false };
  }
  return { baseUrl: text, ok: true };
}

async function confirmAddPreset() {
  if (ui.modal?.t !== "addPreset") return;
  const m = ui.modal;
  const name = requireField(m.name, "preset name is required");
  if (!name) return;
  const parsed = parseBaseUrlField(m);
  if (!parsed.ok) {
    draw();
    return;
  }
  m.urlError = null;
  try {
    await ipc.addPreset(m.projectId, name, parsed.baseUrl, m.danger);
    ui.error = null;
    ui.modal = null;
  } catch (e) {
    m.urlError = String(e);
  }
  await refresh();
}

async function confirmEditPreset() {
  if (ui.modal?.t !== "editPreset") return;
  const m = ui.modal;
  const name = requireField(m.name, "preset name is required");
  if (!name) return;
  const parsed = parseBaseUrlField(m);
  if (!parsed.ok) {
    draw();
    return;
  }
  m.urlError = null;
  try {
    // Create the replacement first, re-point active_preset at it if the
    // edited preset was live, THEN remove the old one - so there is never a
    // moment with zero presets or a silently-reverted active target.
    const created = await ipc.addPreset(m.projectId, name, parsed.baseUrl, m.danger);
    if (m.wasActive) {
      await ipc.setActivePreset(m.projectId, created.id);
    }
    await ipc.removePreset(m.projectId, m.presetId);
    ui.error = null;
    ui.modal = null;
  } catch (e) {
    m.urlError = String(e);
  }
  await refresh();
}

function presetDangerField(m: PresetModal): TemplateResult {
  return html`
    <label class="detect-row">
      <input
        type="checkbox"
        .checked=${m.danger}
        @change=${(e: Event) => {
          m.danger = (e.target as HTMLInputElement).checked;
          draw();
        }}
      />
      <span>Mark as dangerous (e.g. a prod-like target) - shown as a persistent warning, never blocks switching</span>
    </label>
  `;
}

function presetUrlError(m: PresetModal): TemplateResult | typeof nothing {
  return fieldError(m.urlError);
}

// Shared body for the add/edit-preset modals, which differ only in title,
// confirm callback, and primary button label.
function presetModalBody(
  m: PresetModal,
  opts: { title: string; onConfirm: () => void; confirmLabel: string },
): TemplateResult {
  return html`
    <div class="overlay">
      <div class="dialog">
        <h3>${opts.title}</h3>
        <div class="field-row">
          <label>Name</label>
          <input
            placeholder="staging"
            .value=${m.name}
            @input=${(e: Event) => (m.name = (e.target as HTMLInputElement).value)}
          />
        </div>
        <div class="field-row">
          <label>Base URL</label>
          <input
            placeholder="http://localhost:9000"
            .value=${m.baseUrl}
            @input=${(e: Event) => {
              m.baseUrl = (e.target as HTMLInputElement).value;
              m.urlError = null;
              draw();
            }}
          />
        </div>
        ${presetUrlError(m)}
        ${presetDangerField(m)}
        <div class="dialog-actions">
          <button @click=${closeModal}>Cancel</button>
          <button class="primary" @click=${opts.onConfirm}>${opts.confirmLabel}</button>
        </div>
      </div>
    </div>
  `;
}

export function addPresetModal(m: Extract<Modal, { t: "addPreset" }>): TemplateResult {
  return presetModalBody(m, {
    title: "Add preset",
    onConfirm: () => void confirmAddPreset(),
    confirmLabel: "Add",
  });
}

export function editPresetModal(m: Extract<Modal, { t: "editPreset" }>): TemplateResult {
  return presetModalBody(m, {
    title: "Edit preset",
    onConfirm: () => void confirmEditPreset(),
    confirmLabel: "Save",
  });
}
