// Field builders shared by the add/edit-command modals: the env textarea, the
// port override input (plus its validation), and the FE/BE/None role select.
// Split out of ./modals so that module's modal shells + switch stay readable.

import { html, type TemplateResult } from "lit-html";
import { draw, type Modal } from "./state";
import { fieldError } from "./helpers";

// The two modals that carry a free-text `cmd` to validate (add + edit).
export type CmdModal = Extract<Modal, { t: "addCommand" } | { t: "editCommand" }>;

// Parse a modal's raw port-field text into an override for the IPC call.
// Empty text = auto-assign (null). A non-numeric entry is rejected locally
// (inline, via `portError`) before ever reaching the backend; range and
// collision checks are the backend's job (`ports::PortRegistry::project_port`)
// since only it knows what's currently reserved.
export function parsePortField(m: CmdModal): { fixedPort: number | null; ok: true } | { ok: false } {
  const text = m.port.trim();
  if (!text) return { fixedPort: null, ok: true };
  const n = Number(text);
  if (!Number.isInteger(n) || n < 1 || n > 65535) {
    m.portError = "port must be a whole number between 1 and 65535";
    return { ok: false };
  }
  return { fixedPort: n, ok: true };
}

// Optional per-command env overrides, one KEY=VALUE per line. Values may
// reference existing vars via ${NAME} / %NAME% (so PATH=...;%PATH% prepends).
// Lets a command reach a toolchain the inherited env can't (e.g. node past the
// nvm4w symlink) without a hand-rolled wrapper script.
export function envField(m: CmdModal): TemplateResult {
  return html`
    <div class="field-row env-row">
      <label>Env</label>
      <textarea
        class="env-input"
        rows="2"
        spellcheck="false"
        placeholder="optional - KEY=VALUE per line, e.g. PATH=C:\\node\\dir;%PATH%"
        .value=${m.env}
        @input=${(e: Event) => (m.env = (e.target as HTMLTextAreaElement).value)}
      ></textarea>
    </div>
  `;
}

// Manual port override, shown next to the "assign a dynamic port" checkbox on
// both add and edit command modals (only while that checkbox is on - the
// field is meaningless otherwise). Empty = auto-assign from the project's
// port block; a typed value is validated by the backend (range + not already
// reserved by a different command) and any rejection surfaces right below
// the field via `portError`, rather than the shared error banner.
export function portField(m: CmdModal): TemplateResult {
  return html`
    <div class="field-row">
      <label>Port</label>
      <input
        placeholder="auto"
        inputmode="numeric"
        .value=${m.port}
        @input=${(e: Event) => {
          m.port = (e.target as HTMLInputElement).value;
          m.portError = null;
          draw();
        }}
      />
    </div>
    ${fieldError(m.portError)}
  `;
}

// 3-way FE/BE/None role selector, shown on both add and edit command modals.
// Feeds the optional Role badge shown next to a command everywhere in the
// dashboard (Home's running-now rows, Project screen's command rows).
export function roleField(m: CmdModal): TemplateResult {
  return html`
    <div class="field-row">
      <label>Role</label>
      <select
        .value=${m.role ?? ""}
        @change=${(e: Event) => {
          const v = (e.target as HTMLSelectElement).value;
          m.role = v === "FE" || v === "BE" ? v : null;
        }}
      >
        <option value="">None</option>
        <option value="FE">Frontend</option>
        <option value="BE">Backend</option>
      </select>
    </div>
  `;
}
