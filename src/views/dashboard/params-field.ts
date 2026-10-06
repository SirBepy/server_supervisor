// Add/edit-command modal's "Parameters" section: author named axes
// (`CommandParam`) a command's `cmd` template varies along, e.g. a Flutter
// `{DEVICE}` picked between Chrome/Android/iOS. Substitution itself lives
// backend-side (`supervisor::param_sub`); this module only authors the data
// the backend already knows how to resolve.
//
// Split out of ./modal-fields so that file (and ./modals) stay focused on the
// simple single-value fields; params need their own nested-list render logic.

import { html, nothing, type TemplateResult } from "lit-html";
import { draw } from "./state";
import { fieldError } from "./helpers";
import type { CmdModal } from "./modal-fields";
import type { CommandParam, ParamValue } from "../../types/ipc.generated";
import { suggestedParamsForCmd, suggestionToParam, uniqueSlug } from "./param-vocab";

import "./params-field.css";

function blankParam(): CommandParam {
  return { name: "", label: "", values: [{ value: "", label: "", flag: "" }], last_value: null };
}

function blankValue(): ParamValue {
  return { value: "", label: "", flag: "" };
}

// A value row's id is slugged from its label the first time the label goes
// from empty to non-empty, then left alone - so continuing to edit the label
// afterward never orphans a `last_value` pointed at the original id. An
// existing value loaded from the backend already has a non-empty `value`, so
// it never re-enters this branch.
function onValueLabelInput(param: CommandParam, row: ParamValue, newLabel: string) {
  row.label = newLabel;
  if (!row.value) {
    const used = new Set(param.values.filter((v) => v !== row).map((v) => v.value));
    row.value = uniqueSlug(newLabel, used);
  }
}

function valueRow(param: CommandParam, row: ParamValue, onRemove: () => void): TemplateResult {
  return html`
    <div class="param-value-row">
      <input
        class="param-value-label"
        placeholder="Label"
        .value=${row.label}
        @input=${(e: Event) => {
          onValueLabelInput(param, row, (e.target as HTMLInputElement).value);
          draw();
        }}
      />
      <input
        class="param-value-flag"
        placeholder="-d chrome"
        .value=${row.flag}
        @input=${(e: Event) => (row.flag = (e.target as HTMLInputElement).value)}
      />
      <button class="icon-btn" title="Remove value" @click=${onRemove}>
        <i class="ph ph-x"></i>
      </button>
    </div>
  `;
}

function paramBlock(m: CmdModal, param: CommandParam, index: number): TemplateResult {
  const token = param.name.trim() ? param.name.trim().toUpperCase() : "NAME";
  return html`
    <div class="param-block">
      <div class="param-row-top">
        <div class="param-name-col">
          <input
            class="param-name"
            placeholder="device"
            .value=${param.name}
            @input=${(e: Event) => {
              param.name = (e.target as HTMLInputElement).value;
              draw();
            }}
          />
          <span class="param-hint">Use {${token}} in the command</span>
        </div>
        <input
          class="param-label"
          placeholder="Label"
          .value=${param.label}
          @input=${(e: Event) => (param.label = (e.target as HTMLInputElement).value)}
        />
        <button
          class="icon-btn"
          title="Remove parameter"
          @click=${() => {
            m.params.splice(index, 1);
            draw();
          }}
        >
          <i class="ph ph-trash"></i>
        </button>
      </div>
      <div class="param-values">
        ${param.values.map((v, vi) =>
          valueRow(param, v, () => {
            param.values.splice(vi, 1);
            draw();
          }),
        )}
        <button
          class="ghost param-add-value"
          @click=${() => {
            param.values.push(blankValue());
            draw();
          }}
        >
          <i class="ph ph-plus"></i> Add value
        </button>
      </div>
    </div>
  `;
}

function suggestionButton(m: CmdModal): TemplateResult | typeof nothing {
  const suggestions = suggestedParamsForCmd(m.cmd);
  if (suggestions.length === 0) return nothing;
  const used = new Set(m.params.map((p) => p.name.trim().toLowerCase()));
  const next = suggestions.find((s) => !used.has(s.name.toLowerCase()));
  if (!next) return nothing;
  return html`
    <button
      class="ghost"
      @click=${() => {
        m.params.push(suggestionToParam(next));
        draw();
      }}
    >
      <i class="ph ph-sparkle"></i> Suggested parameter
    </button>
  `;
}

export function paramsField(m: CmdModal): TemplateResult {
  return html`
    <div class="field-row params-row">
      <label>Parameters</label>
      <div class="params-section">
        ${m.params.map((p, i) => paramBlock(m, p, i))}
        <div class="params-actions">
          <button
            class="ghost"
            @click=${() => {
              m.params.push(blankParam());
              draw();
            }}
          >
            <i class="ph ph-plus"></i> Add parameter
          </button>
          ${suggestionButton(m)}
        </div>
        ${fieldError(m.paramsError)}
      </div>
    </div>
  `;
}

// Save-time validation for the whole Parameters section. Returns the first
// violation found (authoring order), else null. Mirrors the backend's own
// `validate_params` (`src-tauri/src/supervisor/crud/params.rs`) plus the one
// check only the UI can make cheaply: a param whose `{NAME}` token is absent
// from the cmd text would silently do nothing at spawn time.
export function validateParamsForSave(cmd: string, params: CommandParam[]): string | null {
  const seenNames = new Set<string>();
  for (const p of params) {
    const name = p.name.trim();
    if (!name) return "every parameter needs a name";
    if (!/^[A-Za-z0-9_-]+$/.test(name)) {
      return `param name "${p.name}" must contain only letters, digits, "_", or "-"`;
    }
    if (name.toLowerCase() === "port") {
      return 'param name "port" is reserved (would shadow {PORT})';
    }
    const key = name.toLowerCase();
    if (seenNames.has(key)) return `duplicate param name "${name}"`;
    seenNames.add(key);
    if (p.values.length === 0) return `param "${name}" needs at least one value`;
    for (const v of p.values) {
      if (!v.label.trim()) return `param "${name}" has a value with no label`;
    }
    const token = `{${name.toUpperCase()}}`;
    if (!cmd.includes(token)) {
      return `command text has no ${token} placeholder for param "${name}", so it would do nothing`;
    }
  }
  return null;
}
