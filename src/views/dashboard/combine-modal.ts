// "Combine into one command" migration modal: the dev hand-picks 2+ of a
// project's existing commands and merges them into one new parameterized
// command, one `ParamValue` per source. There is no clustering/similarity
// code anywhere in here - the checkbox list is the only way a source command
// is ever selected.

import "./combine-modal.css";
import { html, nothing, type TemplateResult } from "lit-html";
import type { Command, CommandParam, Project } from "../../types/ipc.generated";
import * as ipc from "../../shared/ipc";
import { ui, draw, refresh, closeModal, type Modal } from "./state";
import { fieldError, resolvedCmdText } from "./helpers";
import { uniqueSlug } from "./param-vocab";
import { validateParamsForSave } from "./params-field";
import {
  diffCommandTokens,
  templateFromTokens,
  flagsFromMiddles,
  labelsFromFlags,
  guessAxisName,
} from "./combine-diff";

type M = Extract<Modal, { t: "combineCommands" }>;

export function startCombineCommands(projectId: string) {
  ui.modal = {
    t: "combineCommands",
    projectId,
    checked: new Set(),
    axisName: "variant",
    axisNameEdited: false,
    label: "Variant",
    template: "",
    templateEdited: false,
    flags: {},
    labels: {},
    error: null,
  };
  draw();
}

function project(m: M): Project | undefined {
  return ui.projects.find((p) => p.id === m.projectId);
}

// Ticked commands, in PROJECT order (never click order) - value index i must
// line up with source index i, both here and in the submitted call.
function tickedCommands(m: M): Command[] {
  return project(m)?.commands.filter((c) => m.checked.has(c.id)) ?? [];
}

function isRunning(id: string): boolean {
  const status = ui.statusById[id]?.status;
  return status === "running" || status === "starting";
}

// Recomputes the diff pre-fill for the CURRENT ticked set. Only the
// axis-name/template fields have a hand-edit lock (axisNameEdited/
// templateEdited): flags/labels don't need one because every row's
// differing span is a function of the whole ticked set - adding or removing
// a command invalidates all of them at once, so there is nothing stable left
// to preserve.
function recompute(m: M) {
  const cmds = tickedCommands(m);
  if (cmds.length < 2) {
    m.flags = {};
    m.labels = {};
    return;
  }
  const { prefix, suffix, middles } = diffCommandTokens(cmds.map((c) => c.cmd));
  const flags = flagsFromMiddles(middles);
  const labels = labelsFromFlags(flags, cmds.map((c) => c.name));
  m.flags = {};
  m.labels = {};
  cmds.forEach((c, i) => {
    m.flags[c.id] = flags[i];
    m.labels[c.id] = labels[i];
  });
  if (!m.axisNameEdited) {
    m.axisName = guessAxisName(flags);
    m.label = m.axisName.charAt(0).toUpperCase() + m.axisName.slice(1);
  }
  if (!m.templateEdited) {
    m.template = templateFromTokens(prefix, suffix, m.axisName);
  }
}

function toggleChecked(m: M, id: string) {
  if (m.checked.has(id)) m.checked.delete(id);
  else m.checked.add(id);
  recompute(m);
  draw();
}

function buildParam(m: M): CommandParam {
  const used = new Set<string>();
  return {
    name: m.axisName.trim(),
    label: m.label.trim() || m.axisName.trim(),
    last_value: null,
    values: tickedCommands(m).map((c) => {
      const label = m.labels[c.id] ?? "";
      return { value: uniqueSlug(label || c.name, used), label, flag: m.flags[c.id] ?? "" };
    }),
  };
}

function validationError(m: M): string | null {
  const cmds = tickedCommands(m);
  if (cmds.length < 2) return "pick at least 2 commands to combine";
  const running = cmds.filter((c) => isRunning(`${m.projectId}:${c.id}`)).map((c) => c.name);
  if (running.length) return `stop these commands before combining: ${running.join(", ")}`;
  return validateParamsForSave(m.template, [buildParam(m)]);
}

async function confirmCombine(m: M) {
  const err = validationError(m);
  if (err) {
    m.error = err;
    draw();
    return;
  }
  const ticked = tickedCommands(m);
  const sourceIds = ticked.map((c) => c.id);
  // The merged command keeps the first source's name like every other
  // non-cmd field; it is renameable afterwards from the edit modal.
  const name = ticked[0].name;
  try {
    await ipc.combineCommands(m.projectId, sourceIds, name, m.template.trim(), buildParam(m));
    ui.modal = null;
    ui.error = null;
    await refresh();
  } catch (e) {
    m.error = String(e);
    draw();
  }
}

function commandCheckboxRow(m: M, project: Project, c: Command): TemplateResult {
  const id = `${project.id}:${c.id}`;
  const running = isRunning(id);
  const info = ui.statusById[id];
  return html`
    <label class="detect-row combine-row">
      <input type="checkbox" .checked=${m.checked.has(c.id)} .disabled=${running} @change=${() => toggleChecked(m, c.id)} />
      <span class="combine-row-name">${c.name}</span>
      <code class="combine-row-cmd">${resolvedCmdText(c, info)}</code>
      ${running ? html`<span class="muted combine-row-note">running, stop it first</span>` : nothing}
    </label>
  `;
}

function axisFields(m: M): TemplateResult {
  return html`
    <div class="field-row">
      <label>Name</label>
      <input
        .value=${m.axisName}
        @input=${(e: Event) => {
          m.axisName = (e.target as HTMLInputElement).value;
          m.axisNameEdited = true;
          if (!m.templateEdited) {
            const cmds = tickedCommands(m);
            const { prefix, suffix } = diffCommandTokens(cmds.map((c) => c.cmd));
            m.template = templateFromTokens(prefix, suffix, m.axisName);
          }
          draw();
        }}
      />
    </div>
    <div class="field-row">
      <label>Label</label>
      <input .value=${m.label} @input=${(e: Event) => (m.label = (e.target as HTMLInputElement).value)} />
    </div>
  `;
}

function templateField(m: M): TemplateResult {
  return html`
    <div class="field-row">
      <label>Template</label>
      <input
        .value=${m.template}
        @input=${(e: Event) => {
          m.template = (e.target as HTMLInputElement).value;
          m.templateEdited = true;
          draw();
        }}
      />
    </div>
  `;
}

function sourceRow(m: M, c: Command): TemplateResult {
  return html`
    <div class="combine-source-row">
      <span class="combine-source-name">${c.name}</span>
      <input
        class="combine-source-label"
        placeholder="Label"
        .value=${m.labels[c.id] ?? ""}
        @input=${(e: Event) => (m.labels[c.id] = (e.target as HTMLInputElement).value)}
      />
      <input
        class="combine-source-flag"
        placeholder="-d chrome"
        .value=${m.flags[c.id] ?? ""}
        @input=${(e: Event) => (m.flags[c.id] = (e.target as HTMLInputElement).value)}
      />
    </div>
  `;
}

// Fields copied from the first ticked source (decision 9's "FIRST source"):
// listed here is whichever of these the ticked commands actually disagree
// on, so nothing resets to a default the dev never saw.
const NON_CMD_FIELDS: { key: keyof Command; label: string }[] = [
  { key: "autostart", label: "Autostart" },
  { key: "use_dynamic_port", label: "Dynamic port" },
  { key: "fixed_port", label: "Fixed port" },
  { key: "env", label: "Env" },
  { key: "role", label: "Role" },
  { key: "dock_window", label: "Dock window" },
  { key: "play_sound", label: "Play sound" },
  { key: "dock_headless", label: "Headless" },
];

function fmtFieldValue(v: unknown): string {
  if (v == null || v === "") return "(none)";
  if (typeof v === "boolean") return v ? "on" : "off";
  return String(v);
}

function reviewBlock(cmds: Command[]): TemplateResult {
  const first = cmds[0];
  const diffs = NON_CMD_FIELDS.filter(({ key }) => cmds.some((c) => c[key] !== first[key]));
  return html`
    <div class="section-label">Review</div>
    ${diffs.length === 0
      ? html`<p class="muted note">No other differences.</p>`
      : html`<ul class="combine-review-list">
          ${diffs.map(
            ({ key, label }) => html`<li>${label}: merged command gets <code>${fmtFieldValue(first[key])}</code></li>`,
          )}
        </ul>`}
    <p class="muted note">
      These commands will be removed: ${cmds.map((c) => `${c.name} (${c.id})`).join(", ")}.
    </p>
    <p class="muted note">
      Their logs and port history do not carry over - anything still holding one of these ids will get "not found".
    </p>
  `;
}

export function combineCommandsModal(m: M): TemplateResult {
  const p = project(m);
  if (!p) {
    return html`<div class="overlay"><div class="dialog"><p class="list-empty">Unknown project.</p></div></div>`;
  }
  const cmds = tickedCommands(m);
  const ready = cmds.length >= 2;
  return html`
    <div class="overlay">
      <div class="dialog combine-dialog">
        <h3>Combine commands</h3>
        <div class="combine-checklist">${p.commands.map((c) => commandCheckboxRow(m, p, c))}</div>
        ${ready
          ? html`
              ${axisFields(m)}
              ${templateField(m)}
              <div class="combine-sources">${cmds.map((c) => sourceRow(m, c))}</div>
              ${reviewBlock(cmds)}
            `
          : nothing}
        ${fieldError(m.error)}
        <div class="dialog-actions">
          <button @click=${closeModal}>Cancel</button>
          <button class="danger" .disabled=${!ready} @click=${() => void confirmCombine(m)}>
            Combine ${m.checked.size} command${m.checked.size === 1 ? "" : "s"}
          </button>
        </div>
      </div>
    </div>
  `;
}
