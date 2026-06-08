// Dashboard controller: mounts the view, drives the poll loop, and renders the
// project/command list. Shared view state + the render trigger live in ./state;
// the modals (add project/command, edit, delete) live in ./modals.

import { html, render, nothing, type TemplateResult } from "lit-html";
import "./dashboard.css";
import * as ipc from "../../shared/ipc";
import type { Project } from "../../types/ipc.generated";
import { ui, setDraw, refresh, act, draw } from "./state";
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

  // Close the per-project "more options" menu on any outside click or Escape.
  // The button + menu both stopPropagation, so any click reaching the document
  // is outside the open menu.
  const onDocClick = () => {
    if (ui.openMenuFor !== null) {
      ui.openMenuFor = null;
      draw();
    }
  };
  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape" && ui.openMenuFor !== null) {
      ui.openMenuFor = null;
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

function totalRunning(): number {
  return ui.projects.reduce((sum, p) => sum + runningCount(p), 0);
}

// Stop-all entry point: one running proc stops immediately; 2+ asks for an
// inline confirm first (mass-stop is easy to fat-finger).
function stopAll() {
  const n = totalRunning();
  if (n === 0) return;
  if (n === 1) {
    void act(ipc.stopAllProcs());
    return;
  }
  ui.modal = { t: "confirmStopAll", count: n };
  draw();
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

// Copy the local URL for a running command's port to the clipboard, with a
// brief "copied" flash on the badge. Always http: these are localhost dev
// servers (and the flutter reload proxy), never https.
function copyPortUrl(id: string, port: number) {
  void navigator.clipboard?.writeText(`http://localhost:${port}`);
  ui.copiedPortId = id;
  draw();
  window.setTimeout(() => {
    if (ui.copiedPortId === id) {
      ui.copiedPortId = null;
      draw();
    }
  }, 1200);
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
          ${pid != null
            ? html`<span class="pid">pid ${pid}</span>`
            : ui.statusById[id]?.status && ui.statusById[id].status !== "stopped"
              ? html`<span class="pid">${ui.statusById[id].status}</span>`
              : nothing}
          ${port != null
            ? html`<button
                class="port ${ui.copiedPortId === id ? "copied" : ""}"
                title="Copy http://localhost:${port}"
                @click=${() => copyPortUrl(id, port)}
              >
                ${ui.copiedPortId === id ? "copied!" : html`port ${port}`}
              </button>`
            : nothing}
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

// Per-project "more options" (kebab) button + its popover menu. Replaces the
// old per-project + button: hover-reveals with .group-actions, and on click
// opens a small anchored menu (Add command / Open in file explorer).
function moreMenu(project: Project): TemplateResult {
  const open = ui.openMenuFor === project.id;
  return html`
    <div class="group-actions ${open ? "menu-open" : ""}">
      <button
        class="more-btn ${open ? "active" : ""}"
        title="More options"
        @click=${(e: Event) => {
          e.stopPropagation();
          ui.openMenuFor = open ? null : project.id;
          draw();
        }}
      >
        <i class="ph ph-dots-three-vertical"></i>
      </button>
      ${open
        ? html`
            <div class="more-menu" @click=${(e: Event) => e.stopPropagation()}>
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
  if (ui.filterRunning && count === 0) return nothing;

  const collapsed = ui.collapsed.has(project.id);
  const visibleCmds = ui.filterRunning
    ? project.commands.filter(
        (c) => ui.statusById[`${project.id}:${c.id}`]?.status === "running",
      )
    : project.commands;

  return html`
    <section class="group">
      <div class="group-head" @click=${() => toggleCollapse(project.id)}>
        <i class="ph ph-caret-right group-chevron ${collapsed ? "" : "open"}"></i>
        <div class="titles">
          <h2>${project.name}</h2>
        </div>
        ${count > 0
          ? html`<span class="run-count"><span class="run-dot"></span>${count}</span>`
          : nothing}
        ${!ui.filterRunning ? moreMenu(project) : nothing}
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
          ${totalRunning() > 0
            ? html`<button class="stop-all-chip" title="Stop all running processes" @click=${stopAll}>
                <i class="ph ph-stop"></i> Stop all
              </button>`
            : nothing}
        </div>
      </div>
      ${ui.error ? html`<div class="error">${ui.error}</div>` : nothing}
      ${emptyMsg}
      ${ui.projects.map(projectSection)}
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
