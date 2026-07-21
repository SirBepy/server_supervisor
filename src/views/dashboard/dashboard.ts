// Dashboard controller: mounts the view, drives the poll loop, and renders the
// two-screen dashboard (Home: stats + running-now + project browser; Project:
// one project's commands + shared detail pane). Shared view state + the render
// trigger live in ./state; the modals (add project/command, edit, delete) live
// in ./modals.

import { html, render, nothing, type TemplateResult } from "lit-html";
import "./dashboard.css";
import * as ipc from "../../shared/ipc";
import type { Group, Project, Role } from "../../types/ipc.generated";
import { ui, setDraw, refresh, act } from "./state";
import { formatBytes, displayName, formatUptime, projectTech, deviconClass, deviconClassByName } from "./helpers";
import { modalView } from "./modals";
import { cmdMenu, groupMenu, moreMenu, portalMenu, setButtonAnchor, setMouseAnchor } from "./menus";
import { renderAnsi } from "../../shared/ansi";

const POLL_MS = 2500;
// "Running now" on Home caps at this many rows total (crashed counted toward
// the cap, not shown in addition to it); searching bypasses the cap entirely.
const RUNNING_CAP = 3;

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
function goToProject(projectId: string) {
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
function toggleSelectCmd(id: string) {
  if (ui.expandedCmdId === id) deselectCmd();
  else selectCmd(id);
}

// Home running-row / jump-bar click: jump straight into the project screen with
// that command already selected, skipping an empty intermediate screen.
function openCommandInProject(projectId: string, id: string) {
  ui.screen = { t: "project", projectId };
  selectCmd(id);
}

// ----- rendering: shared bits -----

function runningCount(project: Project): number {
  return project.commands.filter(
    (c) => ui.statusById[`${project.id}:${c.id}`]?.status === "running",
  ).length;
}

function toggleGroupCollapse(id: string) {
  if (ui.collapsedGroups.has(id)) {
    ui.collapsedGroups.delete(id);
  } else {
    ui.collapsedGroups.add(id);
  }
  draw();
}

function groupRunningCount(group: Group): number {
  return group.project_ids.reduce((sum, pid) => {
    const p = ui.projects.find((p) => p.id === pid);
    return sum + (p ? runningCount(p) : 0);
  }, 0);
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

// Small FE/BE pill next to a name, wherever a command has a declared role.
function roleBadge(role: Role | null | undefined): TemplateResult | typeof nothing {
  if (!role) return nothing;
  return html`<span class="role-badge role-${role.toLowerCase()}">${role}</span>`;
}

// The expanded-card header: pid + RAM + port + uptime, in one muted line.
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

// The project's icon slot, small (row) size.
function projectIconTemplate(project: Project): TemplateResult {
  return html`<span class="picon">${resolveProjectIcon(project)}</span>`;
}

// The project's icon slot, large (Project screen detail header) size.
function projectIconTemplateLg(project: Project): TemplateResult {
  return html`<span class="picon picon-lg">${resolveProjectIcon(project)}</span>`;
}

// ----- Home screen -----

function matchesSearch(project: Project, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  if (project.name.toLowerCase().includes(q)) return true;
  return project.commands.some((c) => c.cmd.toLowerCase().includes(q));
}

function pct(part: number, whole: number): number {
  return whole > 0 ? (part / whole) * 100 : 0;
}

// One project with >=1 running command, for the Home stats strip's "running"
// pill icons: hover reveals the project name, a click jumps to that project
// with its first running command selected (same target as the jump bar and
// running-now rows), and a count badge appears once more than one command
// from that project is running. "First" = first running command in the
// project's own command order, not most-recently-started - simpler and
// deterministic, and matches the order the Project screen itself lists them in.
function runningProjectsSummary(): { project: Project; firstCmdId: string; count: number }[] {
  const out: { project: Project; firstCmdId: string; count: number }[] = [];
  for (const p of ui.projects) {
    let entry: { project: Project; firstCmdId: string; count: number } | null = null;
    for (const c of p.commands) {
      if (ui.statusById[`${p.id}:${c.id}`]?.status !== "running") continue;
      if (!entry) {
        entry = { project: p, firstCmdId: `${p.id}:${c.id}`, count: 1 };
        out.push(entry);
      } else {
        entry.count++;
      }
    }
  }
  return out;
}

const DONUT_SIZE = 76;
const DONUT_STROKE = 12;
const DONUT_R = (DONUT_SIZE - DONUT_STROKE) / 2;
const DONUT_C = 2 * Math.PI * DONUT_R;

// dasharray/dashoffset for the arc spanning [startPct, endPct) of the ring,
// with the whole <g> rotated -90deg so 0% sits at 12 o'clock and grows
// clockwise (see the rotate() on the <g> in donutPill below).
function arcProps(startPct: number, endPct: number): { dasharray: string; dashoffset: number } {
  const len = (Math.max(0, endPct - startPct) / 100) * DONUT_C;
  return { dasharray: `${len} ${DONUT_C - len}`, dashoffset: -((startPct / 100) * DONUT_C) };
}

type DonutMode = "programs" | "system" | "total";
const DONUT_CAPTION: Record<DonutMode, string> = {
  programs: "your apps",
  system: "system used",
  total: "total capacity",
};

function setDonutMode(metric: "ram" | "cpu", mode: DonutMode): void {
  ui.statsDonutMode[metric] = mode;
  draw();
}

// One donut ring: 3 SEPARATE, non-overlapping SVG stroke segments - clicking
// the small accent arc (0 -> appPct) shows the "your apps" reading, the
// middle dim arc (appPct -> sysPct) shows "system used", the outer/remaining
// track arc (sysPct -> 100) shows "total capacity". Each arc's own click
// handler sets that exact reading directly (no cycling) and it stays put
// until a different arc is clicked. `readings` supplies the three possible
// center displays keyed by DonutMode, each with its big value (kept short -
// see the `.small` font fallback) and the hover-tooltip text (the exact
// amount behind that reading).
function donutPill(
  metric: "ram" | "cpu",
  label: string,
  appPct: number,
  sysPct: number,
  readings: Record<DonutMode, { value: string; title: string }>,
): TemplateResult {
  const mode = ui.statsDonutMode[metric];
  const reading = readings[mode];
  const cx = DONUT_SIZE / 2;
  const cy = DONUT_SIZE / 2;
  const app = arcProps(0, appPct);
  const sys = arcProps(appPct, sysPct);
  const track = arcProps(sysPct, 100);

  return html`
    <div class="stat-pill donut-tier">
      <div class="stat-donut-wrap">
        <svg viewBox="0 0 ${DONUT_SIZE} ${DONUT_SIZE}">
          <g transform="rotate(-90 ${cx} ${cy})">
            <circle
              class="arc-seg arc-total"
              cx=${cx}
              cy=${cy}
              r=${DONUT_R}
              fill="none"
              stroke-width=${DONUT_STROKE}
              stroke-dasharray=${track.dasharray}
              stroke-dashoffset=${track.dashoffset}
              @click=${() => setDonutMode(metric, "total")}
            >
              <title>Total capacity</title>
            </circle>
            <circle
              class="arc-seg arc-system"
              cx=${cx}
              cy=${cy}
              r=${DONUT_R}
              fill="none"
              stroke-width=${DONUT_STROKE}
              stroke-dasharray=${sys.dasharray}
              stroke-dashoffset=${sys.dashoffset}
              @click=${() => setDonutMode(metric, "system")}
            >
              <title>System used</title>
            </circle>
            <circle
              class="arc-seg arc-programs"
              cx=${cx}
              cy=${cy}
              r=${DONUT_R}
              fill="none"
              stroke-width=${DONUT_STROKE}
              stroke-dasharray=${app.dasharray}
              stroke-dashoffset=${app.dashoffset}
              @click=${() => setDonutMode(metric, "programs")}
            >
              <title>Your apps</title>
            </circle>
          </g>
        </svg>
        <div class="stat-donut-center">
          <span class="stat-donut-value ${reading.value.length > 4 ? "small" : ""}" title=${reading.title}
            >${reading.value}</span
          >
        </div>
      </div>
      <div class="stat-donut-info">
        <span class="stat-donut-caption">${DONUT_CAPTION[mode]}</span>
        <span class="stat-label">${label}</span>
      </div>
    </div>
  `;
}

function statsStrip(): TemplateResult {
  const runningTotal = ui.projects.reduce((n, p) => n + runningCount(p), 0);
  const stats = ui.systemStats;
  const cores = navigator.hardwareConcurrency || 1;

  // "Your apps": summed across every currently-running command's own sampled
  // figures (mem_bytes/cpu_pct - both real per-process sysinfo samples, see
  // src-tauri/src/supervisor/{mem,cpu}.rs), not the system-wide totals.
  let appMemBytes = 0;
  let appCpuPct = 0;
  for (const info of Object.values(ui.statusById)) {
    if (info.status !== "running") continue;
    if (info.mem_bytes != null) appMemBytes += Number(info.mem_bytes);
    if (info.cpu_pct != null) appCpuPct += info.cpu_pct;
  }

  const sysTotalBytes = stats ? Number(stats.total_mem_bytes) : 0;
  const sysUsedBytes = stats ? Number(stats.used_mem_bytes) : 0;
  const sysCpuPct = stats ? stats.cpu_pct : 0;
  const memAppPct = pct(appMemBytes, sysTotalBytes);
  const memSysPct = pct(sysUsedBytes, sysTotalBytes);
  const cpuAppPct = pct(appCpuPct, 100);
  const cpuSysPct = pct(sysCpuPct, 100);
  const appCoresUsed = (appCpuPct / 100 * cores).toFixed(1);
  const sysCoresUsed = (sysCpuPct / 100 * cores).toFixed(1);

  const runningProjects = runningProjectsSummary();

  return html`
    <div class="stat-strip">
      <div class="stat-pill running-tier">
        <span class="stat-value">${runningTotal}</span>
        <span class="stat-label">running</span>
        ${runningProjects.length
          ? html`
              <div class="run-icons">
                ${runningProjects.map(
                  ({ project, firstCmdId, count }) => html`
                    <span
                      class="run-icon-chip"
                      title=${project.name}
                      @click=${() => openCommandInProject(project.id, firstCmdId)}
                    >
                      ${projectIconTemplate(project)}
                      ${count > 1 ? html`<span class="run-icon-badge">${count}</span>` : nothing}
                    </span>
                  `,
                )}
              </div>
            `
          : nothing}
      </div>

      ${donutPill("ram", "RAM", memAppPct, memSysPct, {
        programs: { value: `${Math.round(memAppPct)}%`, title: formatBytes(appMemBytes) },
        system: { value: `${Math.round(memSysPct)}%`, title: stats ? formatBytes(stats.used_mem_bytes) : "-" },
        // Rounded to whole GB - "23.03 GB" reads as noise when all you want is
        // the ballpark; the exact figure is still one hover away via title.
        total: {
          value: stats ? `${Math.round(sysTotalBytes / 1024 ** 3)} GB` : "-",
          title: stats ? formatBytes(stats.total_mem_bytes) : "-",
        },
      })}

      ${donutPill("cpu", "CPU", cpuAppPct, cpuSysPct, {
        programs: { value: `${Math.round(appCpuPct)}%`, title: `${appCoresUsed} of ${cores} cores` },
        system: { value: `${Math.round(sysCpuPct)}%`, title: `${sysCoresUsed} of ${cores} cores` },
        total: { value: `${cores} cores`, title: "total logical cores" },
      })}
    </div>
  `;
}

function searchBox(): TemplateResult {
  return html`
    <div class="search-box">
      <i class="ph ph-magnifying-glass"></i>
      <input
        type="text"
        placeholder="Find a project..."
        .value=${ui.homeSearch}
        @input=${(e: Event) => {
          ui.homeSearch = (e.target as HTMLInputElement).value;
          draw();
        }}
      />
      ${ui.homeSearch
        ? html`<i
            class="ph ph-x search-clear"
            @click=${() => {
              ui.homeSearch = "";
              draw();
            }}
          ></i>`
        : nothing}
    </div>
  `;
}

function runningRow(project: Project, cmd: Project["commands"][number]): TemplateResult {
  const id = `${project.id}:${cmd.id}`;
  const info = ui.statusById[id];
  const status = info?.status ?? "stopped";
  return html`
    <div class="card run-row ${statusClass(status)}" @click=${() => openCommandInProject(project.id, id)}>
      <div class="row">
        ${projectIconTemplate(project)}
        <div class="row-namecol">
          <span class="row-title">${project.name} ${roleBadge(cmd.role)}</span>
          <span class="row-cmdtext">${cmd.cmd}</span>
        </div>
        ${status === "crashed" ? html`<span class="statusword">crashed</span>` : nothing}
        <div class="right">
          <div class="stats">
            ${ui.showRam && info?.mem_bytes != null
              ? html`<span class="cell"><span class="k">RAM</span><span class="v">${formatBytes(info.mem_bytes)}</span></span>`
              : nothing}
            ${ui.showPort && info?.port != null
              ? html`<span class="cell"><span class="k">Port</span><span class="v">${info.port}</span></span>`
              : nothing}
          </div>
          <i class="ph ph-caret-right row-goto"></i>
        </div>
      </div>
    </div>
  `;
}

function projectBrowseRow(project: Project): TemplateResult {
  const running = runningCount(project);
  return html`
    <div
      class="proj-browse-row"
      @click=${() => goToProject(project.id)}
      @contextmenu=${(e: Event) => {
        e.preventDefault();
        e.stopPropagation();
        ui.openCmdMenuFor = null;
        ui.openMenuFor = project.id;
        setMouseAnchor(e as MouseEvent, 140);
        draw();
      }}
    >
      ${projectIconTemplate(project)}
      <span class="proj-browse-name" title=${project.name}>${project.name}</span>
      <span class="proj-browse-meta">
        <span
          class="meta-chip"
          title="${project.commands.length} command${project.commands.length === 1 ? "" : "s"}"
        >
          <i class="ph ph-terminal-window"></i>${project.commands.length}
        </span>
        <span class="meta-chip ${running > 0 ? "meta-chip-running" : ""}" title="${running} running">
          <i class="ph ph-play-circle"></i>${running}
        </span>
      </span>
      <i class="ph ph-caret-right row-goto"></i>
    </div>
  `;
}

// Groups are the existing shipped mechanism (collapsible, project_ids array),
// re-surfaced on Home after the dashboard-first redesign. Reuses the real
// .grow/.gname/.gbadge/.gchev classes and the same rename/delete/new-project
// context menu the old flat list's group header offered.
function homeGroupSection(group: Group): TemplateResult {
  const members = group.project_ids
    .map((id) => ui.projects.find((p) => p.id === id))
    .filter((p): p is Project => p != null);
  const running = groupRunningCount(group);
  const collapsed = ui.collapsedGroups.has(group.id);
  return html`
    <div
      class="grow"
      @click=${() => toggleGroupCollapse(group.id)}
      @contextmenu=${(e: Event) => {
        e.preventDefault();
        e.stopPropagation();
        ui.openMenuFor = null;
        ui.openCmdMenuFor = null;
        ui.openMoveToGroupFor = null;
        ui.openEmptyMenu = false;
        ui.openGroupMenuFor = group.id;
        setMouseAnchor(e as MouseEvent, 120);
        draw();
      }}
    >
      <i class="ph ${collapsed ? "ph-caret-right" : "ph-caret-down"} gchev"></i>
      <span class="gname">${group.name}</span>
      ${running > 0 ? html`<span class="gbadge">${running} running</span>` : nothing}
      <div @click=${(e: Event) => e.stopPropagation()}>${groupMenu(group)}</div>
    </div>
    ${collapsed ? nothing : html`<div class="group-body">${members.map((p) => projectBrowseRow(p))}</div>`}
  `;
}

function homeScreen(): TemplateResult {
  const searching = ui.homeSearch.trim().length > 0;
  const matchingProjects = ui.projects.filter((p) => matchesSearch(p, ui.homeSearch));

  const allRunning = matchingProjects
    .flatMap((p) => p.commands.map((c) => ({ p, c, info: ui.statusById[`${p.id}:${c.id}`] })))
    .filter(({ info }) => info?.status === "running" || info?.status === "crashed")
    .sort((a, b) => Number(b.info?.started_at ?? 0) - Number(a.info?.started_at ?? 0));
  const hiddenCount = allRunning.length - RUNNING_CAP;
  const visible = searching || ui.showAllRunning || hiddenCount <= 0 ? allRunning : allRunning.slice(0, RUNNING_CAP);

  const groupedIds = new Set(ui.groups.flatMap((g) => g.project_ids));
  const ungrouped = matchingProjects.filter((p) => !groupedIds.has(p.id));

  return html`
    <div class="dash-screen">
      ${statsStrip()}
      ${searchBox()}
      <div class="section-label">Running now</div>
      ${visible.length
        ? html`
            ${visible.map(({ p, c }) => runningRow(p, c))}
            ${!searching && hiddenCount > 0
              ? html`
                  <button
                    class="show-more"
                    @click=${() => {
                      ui.showAllRunning = !ui.showAllRunning;
                      draw();
                    }}
                  >
                    ${ui.showAllRunning ? "Show less" : `Show ${hiddenCount} more`}
                  </button>
                `
              : nothing}
          `
        : html`<p class="list-empty">${searching ? "No running matches." : "Nothing running right now."}</p>`}

      <div class="section-label section-label-secondary">Projects</div>
      ${!matchingProjects.length
        ? ui.projects.length
          ? html`<p class="list-empty">No projects match "${ui.homeSearch}".</p>`
          : html`<p class="list-empty">Right-click to add a project or group.</p>`
        : searching
          ? matchingProjects.map((p) => projectBrowseRow(p))
          : html`
              ${ui.groups.map((g) => homeGroupSection(g))}
              ${ungrouped.map((p) => projectBrowseRow(p))}
            `}
    </div>
  `;
}

// ----- Project screen -----
//
// Commands stay stacked tight as one compact block (no per-row expansion
// breaking up the list). Clicking a live row just selects it (accent
// highlight); a single shared terminal/detail pane renders once, below the
// WHOLE list, reflecting whichever row is selected - not wedged between rows.

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
        ${status === "crashed" ? html`<span class="crashed-tag">crashed</span> ` : nothing}${drawerHeader(info?.pid, info?.mem_bytes, info?.port, info?.started_at)}
      </div>
      <pre class="logs">${ui.logText ? renderAnsi(ui.logText) : "(no output yet)"}</pre>
    </div>
  `;
}

function projectScreen(projectId: string): TemplateResult {
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
