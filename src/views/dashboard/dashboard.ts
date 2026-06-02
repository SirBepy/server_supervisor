// Dashboard controller: mounts the view, drives the poll loop, and renders the
// project/command list. Shared view state + the render trigger live in ./state;
// the modals (add project/command, edit, delete) live in ./modals.

import { html, render, nothing, type TemplateResult } from "lit-html";
import "./dashboard.css";
import * as ipc from "../../shared/ipc";
import type { Project } from "../../types/ipc.generated";
import { ui, setDraw, refresh, act } from "./state";
import { formatBytes, displayName } from "./helpers";
import { modalView, startAddCommand } from "./modals";
import { startAddProject } from "./add-project";
import { renderAnsi } from "../../shared/ansi";

const POLL_MS = 2500;

export function mountDashboard(el: HTMLElement): () => void {
  ui.root = el;
  setDraw(draw);
  void refresh();
  // Capture the poll handle and clear it on teardown. Without this the interval
  // outlives navigation and keeps calling draw() into the (now replaced) root,
  // throwing lit-html "ChildPart has no parentNode" every tick and corrupting
  // whatever view replaced it.
  const timer = window.setInterval(() => void refresh(), POLL_MS);
  return () => window.clearInterval(timer);
}

async function toggleLogs(id: string) {
  if (ui.openLogsFor === id) {
    ui.openLogsFor = null;
    ui.logText = "";
  } else {
    ui.openLogsFor = id;
    ui.logText = (await ipc.getProcLogs(id)).map((l) => l.text).join("\n");
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

function dot(id: string): TemplateResult {
  const status = ui.statusById[id]?.status ?? "stopped";
  return html`<span class="dot ${status}" title=${status}></span>`;
}

function commandRow(project: Project, cmd: Project["commands"][number]): TemplateResult {
  const id = `${project.id}:${cmd.id}`;
  const running = ui.statusById[id]?.status === "running";
  const pid = ui.statusById[id]?.pid;
  const port = ui.statusById[id]?.port;
  const mem = ui.statusById[id]?.mem_bytes;
  return html`
    <div class="card">
      <div class="info">
        <div class="head">
          ${dot(id)}
          <span class="name" title=${cmd.name}>${displayName(cmd)}</span>
        </div>
        <div class="meta">
          <span class="pid">${pid != null ? `pid ${pid}` : ui.statusById[id]?.status ?? "stopped"}</span>
          ${port != null ? html`<span class="port">port ${port}</span>` : nothing}
          ${running && mem != null
            ? html`<span class="ram" title="resident memory (whole process tree)">${formatBytes(mem)}</span>`
            : nothing}
          ${cmd.kind === "flutter" ? html`<span class="tag">flutter</span>` : nothing}
        </div>
      </div>
      <div class="actions">
        ${running
          ? html`
              <button title="Stop" @click=${() => act(ipc.stopProc(id))}>
                <i class="ph ph-stop"></i>
              </button>
              <button title="Restart" @click=${() => act(ipc.restartProc(id))}>
                <i class="ph ph-arrow-clockwise"></i>
              </button>
              ${cmd.kind === "flutter"
                ? html`<button title="Hot restart" @click=${() => act(ipc.reloadProc(id))}>
                    <i class="ph ph-arrows-clockwise"></i>
                  </button>`
                : nothing}
            `
          : html`
              <button title="Start" class="primary" @click=${() => act(ipc.startProc(id))}>
                <i class="ph ph-play"></i>
              </button>
            `}
        <button
          title="Logs"
          class=${ui.openLogsFor === id ? "active" : ""}
          @click=${() => toggleLogs(id)}
        >
          <i class="ph ph-terminal-window"></i>
        </button>
        ${running
          ? nothing
          : html`
              <button
                title="Edit command"
                @click=${() => {
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
                <i class="ph ph-pencil-simple"></i>
              </button>
              <button
                title="Remove command"
                @click=${() => {
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
                <i class="ph ph-trash"></i>
              </button>
            `}
      </div>
    </div>
    ${ui.openLogsFor === id
      ? html`<pre class="logs">${ui.logText ? renderAnsi(ui.logText) : "(no output yet)"}</pre>`
      : nothing}
  `;
}

function projectSection(project: Project): TemplateResult | typeof nothing {
  const count = runningCount(project);
  if (ui.filterRunning && count === 0) return nothing;

  const collapsed = ui.collapsed.has(project.id);
  const visibleCmds = ui.filterRunning
    ? project.commands.filter(
        (c) => ui.statusById[`${project.id}:${c.id}`]?.status === "running",
      )
    : project.commands;

  return html`
    <section class="group ${collapsed ? "collapsed" : ""}">
      <div class="group-head" @click=${() => toggleCollapse(project.id)}>
        <i class="ph ph-caret-right group-chevron ${collapsed ? "" : "open"}"></i>
        <div class="titles">
          <h2>${project.name}</h2>
          <span class="root">${project.root}</span>
        </div>
        ${count > 0
          ? html`<span class="run-count"><span class="run-dot"></span>${count}</span>`
          : nothing}
        ${!ui.filterRunning
          ? html`
              <div class="group-actions">
                <button
                  title="Add command"
                  @click=${(e: Event) => {
                    e.stopPropagation();
                    void startAddCommand(project.id, project.root);
                  }}
                >
                  <i class="ph ph-plus"></i>
                </button>
              </div>
            `
          : nothing}
      </div>
      ${collapsed
        ? nothing
        : visibleCmds.length === 0
          ? html`<p class="empty-cmd">No commands. Add one.</p>`
          : visibleCmds.map((c) => commandRow(project, c))}
    </section>
  `;
}

function draw() {
  const emptyMsg = ui.projects.length === 0
    ? html`<p class="empty">No projects yet. Use the + button to add a project.</p>`
    : ui.filterRunning && ui.projects.every((p) => runningCount(p) === 0)
      ? html`<p class="empty">No running processes.</p>`
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
        <div class="filterbar">
          <button
            class="filter-chip ${ui.filterRunning ? "" : "active"}"
            @click=${() => { ui.filterRunning = false; draw(); }}
          >All</button>
          <button
            class="filter-chip ${ui.filterRunning ? "active" : ""}"
            @click=${() => { ui.filterRunning = true; draw(); }}
          >Running</button>
        </div>
      </div>
      ${ui.error ? html`<div class="error">${ui.error}</div>` : nothing}
      ${emptyMsg}
      ${ui.projects.map(projectSection)}
      ${modalView()}
    `,
    ui.root,
  );
}
