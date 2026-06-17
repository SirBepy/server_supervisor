// Dashboard controller: mounts the view, drives the poll loop, and renders the
// project/command list. Shared view state + the render trigger live in ./state;
// the modals (add project/command, edit, delete) live in ./modals.

import { html, render, nothing, type TemplateResult } from "lit-html";
import "./dashboard.css";
import * as ipc from "../../shared/ipc";
import type { Project } from "../../types/ipc.generated";
import { ui, setDraw, refresh, act } from "./state";
import { formatBytes, displayName, formatUptime, projectTech, deviconClass, deviconClassByName } from "./helpers";
import { modalView, startAddCommand } from "./modals";
import { startAddProject } from "./add-project";
import { renderAnsi } from "../../shared/ansi";

const POLL_MS = 2500;

export function mountDashboard(el: HTMLElement): () => void {
  ui.root = el;
  setDraw(draw);
  void refresh();
  void loadPrefs();
  // Capture the poll handle and clear it on teardown. Without this the interval
  // outlives navigation and keeps calling draw() into the (now replaced) root,
  // throwing lit-html "ChildPart has no parentNode" every tick and corrupting
  // whatever view replaced it.
  const timer = window.setInterval(() => void refresh(), POLL_MS);

  // Close the per-project "more options" menu on any outside click or Escape.
  // The button + menu both stopPropagation, so any click reaching the document
  // is outside the open menu.
  const onDocClick = () => {
    if (ui.openMenuFor !== null || ui.openCmdMenuFor !== null) {
      ui.openMenuFor = null;
      ui.openCmdMenuFor = null;
      draw();
    }
  };
  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape" && (ui.openMenuFor !== null || ui.openCmdMenuFor !== null)) {
      ui.openMenuFor = null;
      ui.openCmdMenuFor = null;
      draw();
    }
  };
  document.addEventListener("click", onDocClick);
  document.addEventListener("keydown", onKey);

  return () => {
    window.clearInterval(timer);
    document.removeEventListener("click", onDocClick);
    document.removeEventListener("keydown", onKey);
  };
}

// Read the density prefs from settings into ui state, then redraw. Runs on every
// dashboard mount - route() remounts the dashboard when returning from #settings,
// so this also picks up changes the user just made without a separate subscription.
async function loadPrefs() {
  try {
    const s = await ipc.getSettings();
    ui.showCommandCount = s.show_command_count;
    ui.showRam = s.show_ram;
    ui.showPort = s.show_port;
    draw();
  } catch {
    // Settings unavailable (e.g. IPC down): keep the defaults already in ui.
  }
}

// Jump-bar click: reveal a running command in the list. Expand its project and
// open its log drawer, then scroll its card into view. The expand + scroll draw
// happens IMMEDIATELY (synchronously); the log text is fetched in the background
// and drawn in when it lands, so a slow getProcLogs never delays the visible
// response. A second click on the already-open command just re-focuses (scrolls)
// without refetching.
function focusCommand(projectId: string, id: string) {
  ui.collapsed.delete(projectId);
  const needLogs = ui.openLogsFor !== id;
  if (needLogs) {
    ui.openLogsFor = id;
    ui.logText = ""; // clear the previous command's text; "(no output yet)" until the fetch lands
    ui.scrollLogsToBottom = true;
  }
  draw();
  // Next frame: the (now expanded) card exists in the DOM, so scroll to it.
  requestAnimationFrame(() => {
    const el = ui.root.querySelector(`[data-cmd-id="${CSS.escape(id)}"]`);
    el?.scrollIntoView({ block: "nearest" });
  });
  if (needLogs) {
    void ipc.getProcLogs(id).then((lines) => {
      // Only apply if this command is still the open one (the user may have
      // clicked elsewhere while the fetch was in flight).
      if (ui.openLogsFor !== id) return;
      ui.logText = lines.map((l) => l.text).join("\n");
      ui.scrollLogsToBottom = true;
      draw();
    });
  }
}

async function toggleLogs(id: string) {
  if (ui.openLogsFor === id) {
    ui.openLogsFor = null;
    ui.logText = "";
  } else {
    ui.openLogsFor = id;
    ui.logText = (await ipc.getProcLogs(id)).map((l) => l.text).join("\n");
    // Newly opened: start at the newest line.
    ui.scrollLogsToBottom = true;
  }
  draw();
}

// ----- rendering -----

function runningCount(project: Project): number {
  return project.commands.filter(
    (c) => ui.statusById[`${project.id}:${c.id}`]?.status === "running",
  ).length;
}

function toggleCollapse(projectId: string) {
  if (ui.collapsed.has(projectId)) {
    ui.collapsed.delete(projectId);
  } else {
    ui.collapsed.add(projectId);
  }
  draw();
}

// Map a process status to the card's status class (drives the colored left
// edge). Unknown/absent reads as stopped.
function statusClass(status: string | undefined): string {
  switch (status) {
    case "running":
    case "starting":
    case "crashed":
      return status;
    default:
      return "stopped";
  }
}

// Open a running command's port in a browser (from the kebab menu). Flutter-web
// ports go to the dedicated CORS-disabled dev browser (new tab in the same
// window); everything else opens in the default browser. Always http: these are
// localhost dev servers (and the flutter reload proxy), never https.
function openInBrowser(port: number, flutter: boolean) {
  void ipc.openPortUrl(`http://localhost:${port}`, flutter);
}

// Copy a command's localhost URL to the clipboard (from the kebab menu).
function copyPortUrl(port: number) {
  void navigator.clipboard?.writeText(`http://localhost:${port}`);
}

// The expanded-card header: pid + RAM + port + uptime, in one muted line. These
// move here from the resting row so the row stays clean; when the card is open
// the right-side stats are hidden, so this is where they remain visible.
function drawerHeader(
  pid: number | null | undefined,
  mem: bigint | number | null | undefined,
  port: number | null | undefined,
  startedAt: bigint | number | null | undefined,
): string {
  const parts: string[] = [];
  if (pid != null) parts.push(`pid ${pid}`);
  if (mem != null) parts.push(formatBytes(mem));
  if (port != null) parts.push(`port ${port}`);
  const up = formatUptime(startedAt);
  if (up) parts.push(`started ${up}`);
  return parts.length ? parts.join(" · ") : "no run info";
}

// Per-command "more options" (kebab) menu. For a live process (running or
// starting): Open-in-browser + Copy URL for its port (the port is no longer a
// bare clickable badge - opening it lives here), then Restart / Stop. For a
// stopped or crashed process: Edit / Remove (you can't edit a live command).
function cmdMenu(
  project: Project,
  cmd: Project["commands"][number],
  id: string,
  status: string,
): TemplateResult {
  const open = ui.openCmdMenuFor === id;
  const live = status === "running" || status === "starting";
  const port = ui.statusById[id]?.port;
  const isFlutter = cmd.kind === "flutter";
  const close = () => {
    ui.openCmdMenuFor = null;
  };
  return html`
    <div class="cmd-more">
      <button
        class=${open ? "active" : ""}
        title="More options"
        @click=${(e: Event) => {
          e.stopPropagation();
          if (!open) {
            const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
            ui.cmdMenuFlipUp = rect.bottom + 200 > window.innerHeight;
          }
          ui.openCmdMenuFor = open ? null : id;
          draw();
        }}
      >
        <i class="ph ph-dots-three-vertical"></i>
      </button>
      ${open
        ? html`
            <div class="more-menu ${ui.cmdMenuFlipUp ? "flip-up" : ""}" @click=${(e: Event) => e.stopPropagation()}>
              ${live
                ? html`
                    ${port != null
                      ? html`
                          <button class="accent" @click=${() => { close(); openInBrowser(port, isFlutter); }}>
                            <i class="ph ph-globe-simple"></i> Open :${port} in browser
                          </button>
                          <button @click=${() => { close(); copyPortUrl(port); }}>
                            <i class="ph ph-copy"></i> Copy URL
                          </button>
                          <div class="menu-div"></div>
                        `
                      : nothing}
                    <button @click=${() => { close(); void act(ipc.restartProc(id)); }}>
                      <i class="ph ph-arrow-clockwise"></i> Restart
                    </button>
                    <button @click=${() => { close(); void act(ipc.stopProc(id)); }}>
                      <i class="ph ph-stop"></i> Stop
                    </button>
                  `
                : html`
                    <button
                      @click=${() => {
                        close();
                        ui.modal = {
                          t: "editCommand",
                          projectId: project.id,
                          commandId: cmd.id,
                          root: project.root,
                          name: cmd.name,
                          cmd: cmd.cmd,
                          autostart: cmd.autostart,
                          useDynamicPort: cmd.use_dynamic_port,
                          env: cmd.env,
                          check: null,
                        };
                        ui.comboOpen = false;
                        draw();
                      }}
                    >
                      <i class="ph ph-pencil-simple"></i> Edit command
                    </button>
                    <button
                      @click=${() => {
                        close();
                        ui.modal = {
                          t: "confirmDeleteCommand",
                          projectId: project.id,
                          commandId: cmd.id,
                          cmdName: cmd.name,
                          lastOne: project.commands.length === 1,
                        };
                        draw();
                      }}
                    >
                      <i class="ph ph-trash"></i> Remove command
                    </button>
                  `}
            </div>
          `
        : nothing}
    </div>
  `;
}

function commandRow(project: Project, cmd: Project["commands"][number]): TemplateResult {
  const id = `${project.id}:${cmd.id}`;
  const info = ui.statusById[id];
  const status = info?.status ?? "stopped";
  const running = status === "running";
  const pid = info?.pid;
  const port = info?.port;
  const mem = info?.mem_bytes;
  const startedAt = info?.started_at;
  const isFlutter = cmd.kind === "flutter";
  const logsOpen = ui.openLogsFor === id;
  // Only live/crashed processes have logs worth expanding; stopped ones are inert.
  const expandable = status !== "stopped";
  const menuOpen = ui.openCmdMenuFor === id;

  return html`
    <div
      data-cmd-id=${id}
      class="card ${statusClass(status)} ${expandable ? "expandable" : ""} ${logsOpen ? "logs-open" : ""} ${menuOpen ? "cmd-menu-open" : ""}"
    >
      <div
        class="row"
        role=${expandable ? "button" : nothing}
        title=${expandable ? `Click to ${logsOpen ? "hide" : "show"} logs` : nothing}
        @click=${expandable ? () => toggleLogs(id) : nothing}
        @contextmenu=${(e: Event) => {
          e.preventDefault();
          e.stopPropagation();
          ui.openMenuFor = null;
          ui.openCmdMenuFor = id;
          draw();
        }}
      >
        <span class="name" title=${cmd.name}>${displayName(cmd)}</span>
        ${isFlutter ? html`<span class="ftag">flutter</span>` : nothing}
        ${status === "crashed" ? html`<span class="statusword">crashed</span>` : nothing}
        ${status === "starting" ? html`<span class="statusword">starting</span>` : nothing}
        <div class="right">
          <div class="stats">
            ${ui.showRam && mem != null
              ? html`<span class="cell"><span class="k">RAM</span><span class="v">${formatBytes(mem)}</span></span>`
              : nothing}
            ${ui.showPort && port != null
              ? html`<span class="cell"><span class="k">Port</span><span class="v">${port}</span></span>`
              : nothing}
          </div>
          <div class="controls" @click=${(e: Event) => e.stopPropagation()}>
            ${running && isFlutter
              ? html`<button class="abtn" title="Hot restart" @click=${() => act(ipc.reloadProc(id))}>
                  <i class="ph ph-arrows-clockwise"></i>
                </button>`
              : nothing}
            ${status === "stopped" || status === "crashed"
              ? html`<button class="abtn start" title="Start" @click=${() => act(ipc.startProc(id))}>
                  <i class="ph ph-play"></i>
                </button>`
              : nothing}
            ${cmdMenu(project, cmd, id, status)}
            ${expandable
              ? html`<i
                  class="ph ${logsOpen ? "ph-caret-up" : "ph-caret-down"} chev"
                  title="${logsOpen ? "Hide" : "Show"} logs"
                  @click=${() => toggleLogs(id)}
                ></i>`
              : nothing}
          </div>
        </div>
      </div>
      ${logsOpen
        ? html`
            <div class="drawer">
              <div class="pidline">${drawerHeader(pid, mem, port, startedAt)}</div>
              <pre class="logs">${ui.logText ? renderAnsi(ui.logText) : "(no output yet)"}</pre>
            </div>
          `
        : nothing}
    </div>
  `;
}

// Kick off a one-time icon fetch for a project, caching the result. Redraws when
// it resolves so the <img> appears. No-op if already fetched/pending.
function ensureProjectIcon(project: Project) {
  if (project.id in ui.iconCache) return;
  ui.iconCache[project.id] = undefined; // mark pending (key now present)
  void ipc
    .getProjectIcon(project.root)
    .then((icon) => {
      ui.iconCache[project.id] = icon ? `data:${icon.mime};base64,${icon.data}` : null;
      draw();
    })
    .catch(() => {
      ui.iconCache[project.id] = null;
      draw();
    });
}

// One-time backend marker-file tech fetch for a project, cached. Only called when
// the command name didn't reveal the tech. No-op if already fetched/pending.
function ensureProjectTech(project: Project) {
  if (project.id in ui.techCache) return;
  ui.techCache[project.id] = undefined; // mark pending (key now present)
  void ipc
    .getProjectTech(project.root)
    .then((tech) => {
      ui.techCache[project.id] = tech;
      draw();
    })
    .catch(() => {
      ui.techCache[project.id] = null;
      draw();
    });
}

// The project's icon, resolved in tiers (the bare <img>/<i>, no wrapper):
//   1. real project icon (backend folder scan)
//   2a. tech logo from the command program (e.g. `cargo`, `flutter`)
//   2b. tech logo from project marker files (e.g. pyproject.toml -> python), for
//       custom launcher commands that hide the tech
//   3. generic Phosphor terminal glyph
// Factored out so both the project row (.picon) and a jump-bar icon (.ji) can
// reuse the same tier logic with their own wrappers.
function resolveProjectIcon(project: Project): TemplateResult {
  ensureProjectIcon(project);
  const cached = ui.iconCache[project.id];
  if (typeof cached === "string") {
    // onerror falls back to the tech logo if the bytes fail to decode.
    return html`<img
      src=${cached}
      alt=""
      @error=${() => {
        ui.iconCache[project.id] = null;
        draw();
      }}
    />`;
  }
  const cmdTech = projectTech(project, ui.statusById);
  if (cmdTech) {
    return html`<i class="${deviconClass(cmdTech)}"></i>`;
  }
  // Command didn't reveal the tech: fall back to the backend marker-file scan.
  ensureProjectTech(project);
  const fileTech = ui.techCache[project.id];
  if (typeof fileTech === "string") {
    const cls = deviconClassByName(fileTech);
    if (cls) return html`<i class="${cls}"></i>`;
  }
  return html`<i class="ph ph-terminal-window"></i>`;
}

// The project's icon slot for the project row.
function projectIconTemplate(project: Project): TemplateResult {
  return html`<span class="picon">${resolveProjectIcon(project)}</span>`;
}

// Per-project "more options" (kebab) button + its popover menu. The kebab trigger
// lives in the row's hover swap zone (.prow-actions); this provides the button +
// anchored menu (Add command / Rename project / Open in file explorer).
function moreMenu(project: Project): TemplateResult {
  const open = ui.openMenuFor === project.id;
  return html`
    <div class="proj-more ${open ? "menu-open" : ""}">
      <button
        class="abtn ${open ? "active" : ""}"
        title="More options"
        @click=${(e: Event) => {
          e.stopPropagation();
          if (!open) {
            const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
            ui.projMenuFlipUp = rect.bottom + 140 > window.innerHeight;
          }
          ui.openMenuFor = open ? null : project.id;
          draw();
        }}
      >
        <i class="ph ph-dots-three-vertical"></i>
      </button>
      ${open
        ? html`
            <div class="more-menu ${ui.projMenuFlipUp ? "flip-up" : ""}" @click=${(e: Event) => e.stopPropagation()}>
              <button
                @click=${() => {
                  ui.openMenuFor = null;
                  void startAddCommand(project.id, project.root);
                }}
              >
                <i class="ph ph-plus"></i> Add command
              </button>
              <button
                @click=${() => {
                  ui.openMenuFor = null;
                  ui.modal = { t: "renameProject", projectId: project.id, name: project.name };
                  draw();
                }}
              >
                <i class="ph ph-pencil-simple"></i> Rename project
              </button>
              <button
                @click=${() => {
                  ui.openMenuFor = null;
                  void ipc.openInExplorer(project.root);
                  draw();
                }}
              >
                <i class="ph ph-folder-open"></i> Open in file explorer
              </button>
            </div>
          `
        : nothing}
    </div>
  `;
}

function projectSection(project: Project): TemplateResult | typeof nothing {
  const count = runningCount(project);
  const collapsed = ui.collapsed.has(project.id);

  return html`
    <section class="group">
      <div
        class="prow"
        @click=${() => toggleCollapse(project.id)}
        @contextmenu=${(e: Event) => {
          e.preventDefault();
          e.stopPropagation();
          ui.openCmdMenuFor = null;
          ui.openMenuFor = project.id;
          draw();
        }}
      >
        <span class="pdot ${count > 0 ? "on" : ""}"></span>
        ${projectIconTemplate(project)}
        <span class="pname" title=${project.name}>${project.name}</span>
        <div class="prow-right" @click=${(e: Event) => e.stopPropagation()}>
          ${ui.showCommandCount
            ? html`<span class="pcount"><i class="ph ph-terminal-window"></i>${project.commands.length}</span>`
            : nothing}
          <div class="prow-actions">${moreMenu(project)}</div>
        </div>
      </div>
      ${collapsed
        ? nothing
        : project.commands.length === 0
          ? html`<p class="empty-cmd">No commands. Add one.</p>`
          : project.commands.map((c) => commandRow(project, c))}
    </section>
  `;
}

// The running jump bar: one icon per live command, pinned under the topbar. A
// pure projection of process state (no own state), hidden entirely when nothing
// is running. Hover shows "project · command"; click reveals it in the list.
function jumpBar(): TemplateResult | typeof nothing {
  const items: { project: Project; cmd: Project["commands"][number]; id: string }[] = [];
  for (const project of ui.projects) {
    for (const cmd of project.commands) {
      const id = `${project.id}:${cmd.id}`;
      const status = ui.statusById[id]?.status;
      // Only live commands have a terminal worth jumping to.
      if (status === "running" || status === "starting") {
        items.push({ project, cmd, id });
      }
    }
  }
  if (items.length === 0) return nothing;
  return html`
    <div class="jump">
      ${items.map(
        ({ project, cmd, id }) => html`
          <button
            class="ji ${ui.openLogsFor === id ? "active" : ""}"
            title=${`${project.name} · ${displayName(cmd)}`}
            @click=${() => void focusCommand(project.id, id)}
          >
            ${resolveProjectIcon(project)}
          </button>
        `,
      )}
    </div>
  `;
}

function draw() {
  const emptyMsg = ui.projects.length === 0
    ? html`<p class="empty">No projects yet. Use the + button to add a project.</p>`
    : nothing;

  render(
    html`
      <div class="header-block">
        <header class="topbar">
          <button class="icon-btn" title="Add project" @click=${() => void startAddProject()}>
            <i class="ph ph-folder-plus"></i>
          </button>
          <h1>Server Supervisor</h1>
          <button class="icon-btn" title="Settings" @click=${() => { location.hash = "#settings"; }}>
            <i class="ph ph-gear"></i>
          </button>
        </header>
        ${jumpBar()}
      </div>
      ${ui.error ? html`<div class="error">${ui.error}</div>` : nothing}
      ${emptyMsg}
      ${ui.projects.map(projectSection)}
      ${ui.projects.length ? html`<div class="list-tail"></div>` : nothing}
      ${modalView()}
    `,
    ui.root,
  );

  // Post-render: if this draw was flagged to pin the log pane (open, or new
  // lines while already at the bottom), scroll it to the newest line. lit-html
  // reuses the <pre> node across renders, so a scrolled-up reader's position is
  // preserved on the draws that do NOT set this flag.
  if (ui.scrollLogsToBottom) {
    const logsEl = ui.root.querySelector<HTMLElement>(".logs");
    if (logsEl) logsEl.scrollTop = logsEl.scrollHeight;
    ui.scrollLogsToBottom = false;
  }
}
