import { html, nothing, type TemplateResult } from "lit-html";
import * as ipc from "../../shared/ipc";
import type { Project, RequestLogEntry, UpstreamPreset } from "../../types/ipc.generated";
import { ui, act, draw } from "./state";
import { toggleSetMember, resolveActivePreset, portUrl } from "./helpers";
import { copyPortUrl } from "./menus";
import { startAddPreset, startEditPreset } from "./preset-modals";

// ----- Reverse-proxy hub: fixed address, preset switcher/management, request log -----

// Kick off a one-time fetch of a project's fixed hub port, caching the result
// like ensureProjectIcon/ensureProjectTech in dashboard.ts. undefined = pending
// (key present, value not yet known), null = fetch failed, number = ready.
function ensureHubPort(projectId: string) {
  if (projectId in ui.hubPort) return;
  ui.hubPort[projectId] = undefined;
  void ipc
    .getHubPort(projectId)
    .then((port) => {
      ui.hubPort[projectId] = port;
      draw();
    })
    .catch(() => {
      ui.hubPort[projectId] = null;
      draw();
    });
}

function confirmRemovePreset(projectId: string, preset: UpstreamPreset) {
  if (window.confirm(`Remove preset "${preset.name}"?`)) {
    void act(ipc.removePreset(projectId, preset.id));
  }
}

// One preset row: a click activates it immediately (no restart implied, no
// confirmation - live switching per the spec) unless it's already active, in
// which case the row is inert (matches the `selectable` pattern on
// projectCmdRow: cursor/click only appear where something would actually
// happen). Edit/remove are separate icon buttons that stop propagation so they
// never also trigger the row's own activate-on-click.
function presetRow(project: Project, preset: UpstreamPreset, activePreset: UpstreamPreset | null): TemplateResult {
  const active = activePreset?.id === preset.id;
  return html`
    <div
      class="preset-row ${active ? "preset-row-active" : "preset-row-clickable"} ${preset.danger ? "preset-row-danger" : ""}"
      @click=${active ? nothing : () => void act(ipc.setActivePreset(project.id, preset.id))}
    >
      <i class="ph ${active ? "ph-check-circle" : "ph-circle"} preset-radio"></i>
      <div class="preset-namecol">
        <span class="preset-name">
          ${preset.name}
          ${preset.danger ? html`<span class="danger-chip"><i class="ph ph-warning"></i>danger</span>` : nothing}
        </span>
        <span class="preset-url">${preset.base_url}</span>
      </div>
      <div class="preset-actions" @click=${(e: Event) => e.stopPropagation()}>
        <button class="abtn" title="Edit preset" @click=${() => startEditPreset(project.id, preset, active)}>
          <i class="ph ph-pencil-simple"></i>
        </button>
        <button class="abtn" title="Remove preset" @click=${() => confirmRemovePreset(project.id, preset)}>
          <i class="ph ph-trash"></i>
        </button>
      </div>
    </div>
  `;
}

function toggleHubLogSection(projectId: string) {
  const opening = !ui.hubLogOpen.has(projectId);
  toggleSetMember(ui.hubLogOpen, projectId);
  if (opening) {
    // Fetch right away on open so the first paint isn't empty until the next
    // poll tick; refresh() in state.ts keeps it live afterwards.
    void ipc.getHubLog(projectId).then((entries) => {
      ui.hubLog[projectId] = entries;
      draw();
    });
  }
  draw();
}

function hubLogRow(entry: RequestLogEntry): TemplateResult {
  const ok = entry.status >= 200 && entry.status < 300;
  const time = new Date(Number(entry.ts)).toLocaleTimeString(undefined, { hour12: false });
  return html`
    <div class="hublog-row ${ok ? "" : "hublog-row-bad"}">
      <span class="hublog-time">${time}</span>
      <span class="hublog-method">${entry.method}</span>
      <span class="hublog-path" title=${entry.path}>${entry.path}</span>
      <span class="hublog-status">${entry.status}</span>
      <span class="hublog-duration">${entry.duration_ms}ms</span>
      <span class="hublog-preset">${entry.preset}</span>
    </div>
  `;
}

// Collapsed-by-default request-log viewer (the hub's capped ring buffer). Same
// collapse idiom as envBlock above (ph-caret-right/down, starts closed).
function requestLogSection(project: Project): TemplateResult {
  const open = ui.hubLogOpen.has(project.id);
  const entries = ui.hubLog[project.id];
  return html`
    <div class="hublog-section">
      <button class="env-toggle" @click=${() => toggleHubLogSection(project.id)}>
        <i class="ph ${open ? "ph-caret-down" : "ph-caret-right"}"></i>
        <span>Request log</span>
      </button>
      ${open
        ? html`
            <div class="hublog-body">
              ${!entries
                ? html`<p class="env-empty">Loading...</p>`
                : entries.length === 0
                  ? html`<p class="env-empty">No requests logged yet.</p>`
                  : html`
                      <div class="hublog-grid">
                        <div class="hublog-row hublog-head">
                          <span>Time</span><span>Method</span><span>Path</span><span>Status</span><span>Duration</span><span>Preset</span>
                        </div>
                        ${entries.map((e) => hubLogRow(e))}
                      </div>
                    `}
            </div>
          `
        : nothing}
    </div>
  `;
}

// The project's proxy hub: its fixed loopback address (copyable, since the dev
// bakes it into his apps' --dart-define/VITE_ config once), the preset
// switcher/manager, and the request-log viewer. A persistent high-contrast
// banner appears whenever the ACTIVE preset is flagged dangerous - purely
// visual per the spec, no confirmation gate anywhere in this section.
export function proxySection(project: Project): TemplateResult {
  ensureHubPort(project.id);
  const port = ui.hubPort[project.id];
  const activePreset = resolveActivePreset(project);
  const dangerActive = activePreset?.danger ?? false;
  return html`
    <div class="proxy-section ${dangerActive ? "proxy-section-danger" : ""}">
      <div class="proxy-hub-row">
        <span class="proxy-hub-label">Hub address</span>
        ${port === undefined
          ? html`<span class="muted">loading...</span>`
          : port === null
            ? html`<span class="muted">unavailable</span>`
            : html`
                <button class="proxy-hub-url" title="Copy to clipboard" @click=${() => copyPortUrl(port)}>
                  <code>${portUrl(port)}</code>
                  <i class="ph ph-copy"></i>
                </button>
              `}
      </div>
      ${dangerActive
        ? html`
            <div class="proxy-danger-banner">
              <i class="ph ph-warning-octagon"></i>
              <span>Active preset "${activePreset!.name}" is marked dangerous - traffic is going to ${activePreset!.base_url}</span>
            </div>
          `
        : nothing}
      <div class="proxy-presets">
        ${project.presets.length === 0
          ? html`<p class="list-empty">No presets yet. Add one to route this project's hub traffic.</p>`
          : project.presets.map((p) => presetRow(project, p, activePreset))}
      </div>
      <button class="proxy-add-preset" @click=${() => startAddPreset(project.id)}>
        <i class="ph ph-plus"></i> Add preset
      </button>
      ${requestLogSection(project)}
    </div>
  `;
}
