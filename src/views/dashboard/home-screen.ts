// Home screen: stats strip (running count + RAM/CPU donuts), search box,
// "running now" list, and the project/group browser. Pulled out of
// dashboard.ts (the mount/poll/navigation controller) along the Home/Project
// seam; shared state (`ui`), the render trigger (`draw`), and controller-owned
// nav/icon helpers are imported from ./state and ./dashboard.

import { html, nothing, type TemplateResult } from "lit-html";
import type { Group, Project } from "../../types/ipc.generated";
import { ui, draw } from "./state";
import { formatBytes, resolveActivePreset } from "./helpers";
import { statusClass, roleBadge, openCommandInProject, goToProject, resolveProjectIcon } from "./dashboard";
import { groupMenu, setMouseAnchor } from "./menus";

// "Running now" on Home caps at this many rows total (crashed counted toward
// the cap, not shown in addition to it); searching bypasses the cap entirely.
const RUNNING_CAP = 3;

// ----- shared bits used only by Home -----

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

// The project's icon slot, small (row) size.
function projectIconTemplate(project: Project): TemplateResult {
  return html`<span class="picon">${resolveProjectIcon(project)}</span>`;
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
              ? html`<span class="cell" title=${info.fallback_port ? "not this command's usual port - its usual one was occupied at launch" : ""}>
                  <span class="k">Port</span><span class="v">${info.port}${info.fallback_port ? html`<i class="ph ph-warning port-fallback-icon"></i>` : nothing}</span>
                </span>`
              : nothing}
          </div>
          <i class="ph ph-caret-right row-goto"></i>
        </div>
      </div>
    </div>
  `;
}

// Compact, informational-only badge for a project whose active proxy preset is
// flagged dangerous - visible without drilling into the project, deliberately
// not a button (no click handler, no new action on the row - the dev has
// twice rejected adding row-level action buttons on Home).
function dangerBadge(project: Project): TemplateResult | typeof nothing {
  const active = resolveActivePreset(project);
  if (!active?.danger) return nothing;
  return html`<span class="meta-chip danger-chip" title="Active preset &quot;${active.name}&quot; is marked dangerous">
    <i class="ph ph-warning"></i>danger
  </span>`;
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
        ${dangerBadge(project)}
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

export function homeScreen(): TemplateResult {
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
