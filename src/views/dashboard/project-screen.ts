// Project screen: one project's commands + the shared detail pane.
//
// Commands stay stacked tight as one compact block (no per-row expansion
// breaking up the list). Clicking a live row just selects it (accent
// highlight); a single shared terminal/detail pane renders once, below the
// WHOLE list, reflecting whichever row is selected - not wedged between rows.
//
// Pulled out of dashboard.ts (the mount/poll/navigation controller) along the
// Home/Project seam; shared state (`ui`), the render trigger (`draw`)/`act`,
// and controller-owned nav/icon helpers are imported from ./state and
// ./dashboard.

import { html, nothing, type TemplateResult } from "lit-html";
import * as ipc from "../../shared/ipc";
import type { EnvVar, ProcInfo, Project, RequestLogEntry, UpstreamPreset } from "../../types/ipc.generated";
import { ui, act, draw } from "./state";
import { formatBytes, formatUptime, displayName, resolveActivePreset, toggleSetMember, portUrl } from "./helpers";
import { statusClass, roleBadge, toggleSelectCmd, resolveProjectIcon } from "./dashboard";
import { cmdMenu, setMouseAnchor, copyPortUrl, openInBrowser } from "./menus";
import { startAddPreset, startEditPreset } from "./preset-modals";
import { renderAnsi } from "../../shared/ansi";

// The project's icon slot, large (Project screen detail header) size.
function projectIconTemplateLg(project: Project): TemplateResult {
  return html`<span class="picon picon-lg">${resolveProjectIcon(project)}</span>`;
}

// The expanded-card header: pid + RAM + port + uptime, in one muted line.
// `fallbackPort` flags a port that isn't the command's usual project-block/
// override port (its usual one was occupied at spawn time - see
// `ProcInfo.fallback_port`), so the dev isn't surprised the URL moved.
function drawerHeader(
  pid: number | null | undefined,
  mem: bigint | number | null | undefined,
  port: number | null | undefined,
  startedAt: bigint | number | null | undefined,
  fallbackPort: boolean | undefined,
): string {
  const parts: string[] = [];
  if (pid != null) parts.push(`pid ${pid}`);
  if (mem != null) parts.push(formatBytes(mem));
  if (port != null) parts.push(fallbackPort ? `port ${port} (not usual)` : `port ${port}`);
  const up = formatUptime(startedAt);
  if (up) parts.push(`started ${up}`);
  return parts.length ? parts.join(" · ") : "no run info";
}

function projectCmdRow(project: Project, cmd: Project["commands"][number]): TemplateResult {
  const id = `${project.id}:${cmd.id}`;
  const info = ui.statusById[id];
  const status = info?.status ?? "stopped";
  const running = status === "running";
  const isFlutter = cmd.kind === "flutter";
  const port = info?.port;
  // Only live/crashed processes have anything to select; stopped ones are inert
  // (no pointer cursor, no hover feedback, no click handler at all).
  const selectable = status !== "stopped";
  const selected = ui.expandedCmdId === id;
  const menuOpen = ui.openCmdMenuFor === id;

  return html`
    <div
      class="card ${statusClass(status)} ${selectable ? "expandable" : ""} ${selected ? "selected" : ""} ${menuOpen ? "cmd-menu-open" : ""}"
      @click=${selectable ? () => toggleSelectCmd(id) : nothing}
      @contextmenu=${(e: Event) => {
        e.preventDefault();
        e.stopPropagation();
        ui.openMenuFor = null;
        ui.openCmdMenuFor = id;
        setMouseAnchor(e as MouseEvent, 200);
        draw();
      }}
    >
      <div class="row">
        <div class="row-namecol">
          <span class="row-title">${displayName(cmd)} ${roleBadge(cmd.role)}</span>
          <span class="row-cmdtext">${cmd.cmd}</span>
        </div>
        ${status === "crashed" ? html`<span class="statusword">crashed</span>` : nothing}
        ${status === "starting" ? html`<span class="statusword">starting</span>` : nothing}
        <div class="right">
          ${running && ui.showPort && port != null
            ? html`
                <button
                  class="meta-chip port-chip"
                  title="Open :${port} in browser"
                  @click=${(e: Event) => {
                    e.stopPropagation();
                    openInBrowser(port, isFlutter);
                  }}
                >
                  <i class="ph ph-arrow-square-out"></i>${port}
                </button>
              `
            : nothing}
          <div class="controls" @click=${(e: Event) => e.stopPropagation()}>
            ${running && isFlutter
              ? html`<button class="abtn" title="Hot restart" @click=${() => act(ipc.reloadProc(id))}>
                  <i class="ph ph-arrows-clockwise"></i>
                </button>`
              : nothing}
            ${status === "stopped" || status === "crashed"
              ? html`<button
                  class="abtn start"
                  title="Start"
                  @click=${() => {
                    if (ui.openLogsFor === id) ui.logText = "";
                    act(ipc.startProc(id));
                  }}
                >
                  <i class="ph ph-play"></i>
                </button>`
              : nothing}
            ${status === "running" || status === "starting"
              ? html`
                  <button
                    class="abtn"
                    title="Restart"
                    @click=${() => {
                      if (ui.openLogsFor === id) ui.logText = "";
                      void act(ipc.restartProc(id));
                    }}
                  >
                    <i class="ph ph-arrow-clockwise"></i>
                  </button>
                  <button
                    class="abtn"
                    title="Stop"
                    @click=${() => {
                      if (ui.openLogsFor === id) ui.logText = "";
                      void act(ipc.stopProc(id));
                    }}
                  >
                    <i class="ph ph-stop"></i>
                  </button>
                `
              : nothing}
            ${cmdMenu(project, cmd, id, status)}
          </div>
        </div>
      </div>
    </div>
  `;
}

function toggleEnvSection(id: string) {
  toggleSetMember(ui.envSectionOpen, id);
  draw();
}

function toggleEnvReveal(revealKey: string) {
  toggleSetMember(ui.envRevealed, revealKey);
  draw();
}

// One key/value line. Secret-looking keys (see `EnvVar.secret`, classified
// once in Rust) render masked behind a click-to-reveal; URL-ish vars - what
// the dev actually needs, to see which backend a running process is pointed
// at - render plainly.
function envRow(id: string, v: EnvVar): TemplateResult {
  const revealKey = `${id}::${v.key}`;
  const revealed = ui.envRevealed.has(revealKey);
  const masked = v.secret && !revealed;
  return html`
    <div class="env-row">
      <span class="env-key">${v.key}</span>
      <span class="env-value ${masked ? "env-masked" : ""}">${masked ? "••••••••" : v.value}</span>
      ${v.secret
        ? html`<button
            class="env-reveal"
            title=${revealed ? "Hide value" : "Reveal value"}
            @click=${() => toggleEnvReveal(revealKey)}
          >
            <i class="ph ${revealed ? "ph-eye-slash" : "ph-eye"}"></i>
          </button>`
        : nothing}
    </div>
  `;
}

// Collapsed-by-default "Environment" block: the resolved env overrides
// actually applied to the current run (parsed `spec.env` plus injected PORT -
// never the full inherited environment or PATH). Distinguishes three states
// the dev must not confuse: a real known env, an explicit "unknown" for a
// re-adopted process (its spawn-time env died with the prior app instance),
// and "not running" for a stopped/crashed command (never a stale env from a
// previous run).
function envBlock(id: string, info: ProcInfo | undefined): TemplateResult | typeof nothing {
  if (!info) return nothing;
  const open = ui.envSectionOpen.has(id);
  const vars = info.resolved_env;
  return html`
    <div class="env-section">
      <button class="env-toggle" @click=${() => toggleEnvSection(id)}>
        <i class="ph ${open ? "ph-caret-down" : "ph-caret-right"}"></i>
        <span>Environment</span>
      </button>
      ${open
        ? html`
            <div class="env-body">
              ${info.env_unknown
                ? html`<p class="env-empty">Unknown - re-adopted after restart. Restart this process to see its resolved env.</p>`
                : vars == null
                  ? html`<p class="env-empty">Not running.</p>`
                  : vars.length === 0
                    ? html`<p class="env-empty">No env overrides for this run.</p>`
                    : html`<div class="env-grid">${vars.map((v) => envRow(id, v))}</div>`}
            </div>
          `
        : nothing}
    </div>
  `;
}

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
function proxySection(project: Project): TemplateResult {
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

function detailPane(project: Project): TemplateResult {
  const cmd = project.commands.find((c) => `${project.id}:${c.id}` === ui.expandedCmdId);
  if (!cmd) {
    return html`<p class="list-empty detail-placeholder">Select a running command above to see its logs.</p>`;
  }
  const id = `${project.id}:${cmd.id}`;
  const info = ui.statusById[id];
  const status = info?.status ?? "stopped";
  return html`
    <div class="detail-pane">
      <div class="detail-cmdline"><i class="ph ph-caret-right"></i> ${cmd.cmd}</div>
      <div class="pidline">
        ${status === "crashed" ? html`<span class="crashed-tag">crashed</span> ` : nothing}${drawerHeader(info?.pid, info?.mem_bytes, info?.port, info?.started_at, info?.fallback_port)}
      </div>
      <pre class="logs">${ui.logText ? renderAnsi(ui.logText) : "(no output yet)"}</pre>
      ${envBlock(id, info)}
    </div>
  `;
}

export function projectScreen(projectId: string): TemplateResult {
  const project = ui.projects.find((p) => p.id === projectId);
  if (!project) {
    return html`<div class="dash-screen"><p class="list-empty">Unknown project.</p></div>`;
  }
  return html`
    <div class="dash-screen">
      <div class="detail-header">
        ${projectIconTemplateLg(project)}
        <div>
          <div class="detail-title">${project.name}</div>
          <div class="detail-sub" title=${project.root}>${project.root}</div>
        </div>
      </div>
      <div class="section-label">Commands</div>
      ${project.commands.length === 0
        ? html`<p class="list-empty">No commands. Add one from the menu above.</p>`
        : html`<div class="cmd-block">${project.commands.map((c) => projectCmdRow(project, c))}</div>`}
      <div class="section-label">Proxy</div>
      ${proxySection(project)}
      ${detailPane(project)}
    </div>
  `;
}
