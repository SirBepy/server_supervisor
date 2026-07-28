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
import type { EnvVar, ProcInfo, Project } from "../../types/ipc.generated";
import { ui, act, draw } from "./state";
import { formatBytes, formatUptime, displayName } from "./helpers";
import { statusClass, roleBadge, toggleSelectCmd, resolveProjectIcon } from "./dashboard";
import { cmdMenu, setMouseAnchor } from "./menus";
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
  if (ui.envSectionOpen.has(id)) ui.envSectionOpen.delete(id);
  else ui.envSectionOpen.add(id);
  draw();
}

function toggleEnvReveal(revealKey: string) {
  if (ui.envRevealed.has(revealKey)) ui.envRevealed.delete(revealKey);
  else ui.envRevealed.add(revealKey);
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
      ${detailPane(project)}
    </div>
  `;
}
