// Dashboard controller: mounts the view, drives the poll loop, and renders the
// two-screen dashboard (Home: stats + running-now + project browser; Project:
// one project's commands + shared detail pane). Shared view state + the render
// trigger live in ./state; the modals (add project/command, edit, delete) live
// in ./modals. Home-screen and Project-screen rendering live in ./home-screen
// and ./project-screen; this file keeps the mount/navigation/selection
// controller plus the icon/status helpers and topbar/jump-bar shared by both.

import { html, render, nothing, type TemplateResult } from "lit-html";
import "./dashboard.css";
import "./home-screen.css";
import "./stats-strip.css";
import "./project-screen.css";
import * as ipc from "../../shared/ipc";
import type { Project, Role } from "../../types/ipc.generated";
import { ui, setDraw, refresh } from "./state";
import { displayName, projectTech, deviconClass, deviconClassByName } from "./helpers";
import { modalView } from "./modals";
import { moreMenu, portalMenu, setButtonAnchor, setMouseAnchor } from "./menus";
import { homeScreen } from "./home-screen";
import { projectScreen } from "./project-screen";

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
    if (
      ui.openMenuFor !== null ||
      ui.openCmdMenuFor !== null ||
      ui.openGroupMenuFor !== null ||
      ui.openMoveToGroupFor !== null ||
      ui.openEmptyMenu
    ) {
      ui.openMenuFor = null;
      ui.openCmdMenuFor = null;
      ui.openGroupMenuFor = null;
      ui.openMoveToGroupFor = null;
      ui.openEmptyMenu = false;
      ui.menuAnchor = null;
      draw();
    }
  };
  const onKey = (e: KeyboardEvent) => {
    if (
      e.key === "Escape" &&
      (ui.openMenuFor !== null ||
        ui.openCmdMenuFor !== null ||
        ui.openGroupMenuFor !== null ||
        ui.openMoveToGroupFor !== null ||
        ui.openEmptyMenu)
    ) {
      ui.openMenuFor = null;
      ui.openCmdMenuFor = null;
      ui.openGroupMenuFor = null;
      ui.openMoveToGroupFor = null;
      ui.openEmptyMenu = false;
      ui.menuAnchor = null;
      draw();
    }
  };
  const onContextMenu = (e: MouseEvent) => {
    if ((e.target as HTMLElement).closest(".card, .grow, .more-menu, .proj-more, .cmd-more, .proj-browse-row")) return;
    // Only Home has an empty-area "new project/group" menu; the Project screen's
    // equivalent actions live in its topbar kebab.
    if (ui.screen.t !== "home") return;
    e.preventDefault();
    ui.openMenuFor = null;
    ui.openCmdMenuFor = null;
    ui.openGroupMenuFor = null;
    ui.openMoveToGroupFor = null;
    ui.openEmptyMenu = true;
    setMouseAnchor(e, 80);
    draw();
  };
  document.addEventListener("click", onDocClick);
  document.addEventListener("keydown", onKey);
  // Listen on document, not el: el (the host div) shrink-wraps its content
  // rather than filling the viewport, so empty space below the list belongs
  // to body, not el. A document-level listener catches right-clicks there too.
  document.addEventListener("contextmenu", onContextMenu);

  return () => {
    window.clearInterval(timer);
    document.removeEventListener("click", onDocClick);
    document.removeEventListener("keydown", onKey);
    document.removeEventListener("contextmenu", onContextMenu);
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

// ----- navigation -----

function goHome() {
  ui.screen = { t: "home" };
  ui.expandedCmdId = null;
  ui.openLogsFor = null;
  ui.logText = "";
  draw();
}

// Navigate to a project's screen with nothing pre-selected (Home's project
// browse rows, and a group/project's own context-menu entries).
export function goToProject(projectId: string) {
  ui.screen = { t: "project", projectId };
  ui.expandedCmdId = null;
  ui.openLogsFor = null;
  ui.logText = "";
  draw();
}

// Select a command (Project screen row click, Home running-row click, jump-bar
// click): fetch its logs and mark it the active selection. ui.openLogsFor stays
// the underlying log-fetch trigger the existing refresh()/act() machinery
// already keys off; ui.expandedCmdId just drives which row is highlighted and
// which command the shared detail pane below the block reflects.
function selectCmd(id: string) {
  ui.expandedCmdId = id;
  ui.openLogsFor = id;
  ui.logText = ""; // clear the previous command's text until the fetch lands
  ui.scrollLogsToBottom = true;
  draw();
  void ipc.getProcLogs(id).then((lines) => {
    // Only apply if this command is still the selected one (the user may have
    // clicked elsewhere while the fetch was in flight).
    if (ui.openLogsFor !== id) return;
    ui.logText = lines.map((l) => l.text).join("\n");
    ui.scrollLogsToBottom = true;
    draw();
  });
}

function deselectCmd() {
  ui.expandedCmdId = null;
  ui.openLogsFor = null;
  ui.logText = "";
  draw();
}

// Project screen row click: single-select, click-to-toggle (matches the
// shipped app's former single-open-drawer behavior).
export function toggleSelectCmd(id: string) {
  if (ui.expandedCmdId === id) deselectCmd();
  else selectCmd(id);
}

// Home running-row / jump-bar click: jump straight into the project screen with
// that command already selected, skipping an empty intermediate screen.
export function openCommandInProject(projectId: string, id: string) {
  ui.screen = { t: "project", projectId };
  selectCmd(id);
}

// ----- rendering: shared bits -----

// Map a process status to the card's status class (drives the colored left
// edge). Unknown/absent reads as stopped.
export function statusClass(status: string | undefined): string {
  switch (status) {
    case "running":
    case "starting":
    case "crashed":
      return status;
    default:
      return "stopped";
  }
}

// Small FE/BE pill next to a name, wherever a command has a declared role.
export function roleBadge(role: Role | null | undefined): TemplateResult | typeof nothing {
  if (!role) return nothing;
  return html`<span class="role-badge role-${role.toLowerCase()}">${role}</span>`;
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
// Factored out so the project row, running-now row, project detail header, and
// jump-bar icon can all reuse the same tier logic with their own wrappers.
export function resolveProjectIcon(project: Project): TemplateResult {
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

// ----- topbar + jump bar -----

function topbar(): TemplateResult {
  const s = ui.screen;
  if (s.t === "home") {
    return html`
      <header class="topbar">
        <h1>Server Supervisor</h1>
        <button class="icon-btn" title="Settings" @click=${() => { location.hash = "#settings"; }}>
          <i class="ph ph-gear"></i>
        </button>
      </header>
    `;
  }
  const project = ui.projects.find((p) => p.id === s.projectId);
  return html`
    <header class="topbar">
      <button class="icon-btn" title="Back" @click=${goHome}>
        <i class="ph ph-arrow-left"></i>
      </button>
      <h1 title=${project?.name ?? ""}>${project?.name ?? ""}</h1>
      ${project ? moreMenu(project) : nothing}
    </header>
  `;
}

// The running jump bar: one icon per live command, pinned under the topbar. A
// pure projection of process state (no own state), hidden entirely when nothing
// is running. Home-only - it complements Home's own running-now list by giving
// uncapped, at-a-glance access to every live command; the Project screen's
// commands are already all visible in its compact block. Hover shows
// "project · command"; click jumps straight into that project with the command
// selected.
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
            class="ji ${ui.expandedCmdId === id ? "active" : ""}"
            title=${`${project.name} · ${displayName(cmd)}`}
            @click=${() => openCommandInProject(project.id, id)}
            @contextmenu=${(e: Event) => {
              e.preventDefault();
              e.stopPropagation();
              ui.openMenuFor = null;
              ui.openCmdMenuFor = id;
              setButtonAnchor(e, 200);
              draw();
            }}
          >
            ${resolveProjectIcon(project)}
          </button>
        `,
      )}
    </div>
  `;
}

// ----- root draw -----

function draw() {
  const s = ui.screen;
  render(
    html`
      <div class="header-block">
        ${topbar()}
        ${s.t === "home" ? jumpBar() : nothing}
      </div>
      ${ui.error ? html`<div class="error">${ui.error}</div>` : nothing}
      ${s.t === "home" ? homeScreen() : projectScreen(s.projectId)}
      ${modalView()}
      ${portalMenu()}
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
