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
import { statsStrip } from "./stats-strip";

// "Running now" on Home caps at this many rows total (crashed counted toward
// the cap, not shown in addition to it); searching bypasses the cap entirely.
const RUNNING_CAP = 3;

// ----- shared bits used only by Home -----

export function runningCount(project: Project): number {
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
export function projectIconTemplate(project: Project): TemplateResult {
  return html`<span class="picon">${resolveProjectIcon(project)}</span>`;
}

// ----- Home screen -----

function matchesSearch(project: Project, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  if (project.name.toLowerCase().includes(q)) return true;
  return project.commands.some((c) => c.cmd.toLowerCase().includes(q));
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
