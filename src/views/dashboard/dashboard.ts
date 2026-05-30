// Dashboard controller: mounts the view, drives the poll loop, and renders the
// project/command list. Shared view state + the render trigger live in ./state;
// the modals (add project/command, edit, delete) live in ./modals.

import { html, render, nothing, type TemplateResult } from "lit-html";
import "./dashboard.css";
import * as ipc from "../../shared/ipc";
import type { Project } from "../../types/ipc.generated";
import { ui, setDraw, refresh, act } from "./state";
import { modalView, startAddProject, startAddCommand } from "./modals";

const POLL_MS = 2500;

export function mountDashboard(el: HTMLElement) {
  ui.root = el;
  setDraw(draw);
  void refresh();
  window.setInterval(() => void refresh(), POLL_MS);
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

function dot(id: string): TemplateResult {
  const status = ui.statusById[id]?.status ?? "stopped";
  return html`<span class="dot ${status}" title=${status}></span>`;
}

function commandRow(project: Project, cmd: Project["commands"][number]): TemplateResult {
  const id = `${project.id}:${cmd.id}`;
  const running = ui.statusById[id]?.status === "running";
  const pid = ui.statusById[id]?.pid;
  const port = ui.statusById[id]?.port;
  return html`
    <div class="card">
      <div class="meta">
        ${dot(id)}
        <span class="name">${cmd.name}</span>
        ${cmd.kind === "flutter" ? html`<span class="tag">flutter</span>` : nothing}
        <span class="pid">${pid != null ? `pid ${pid}` : ui.statusById[id]?.status ?? "stopped"}</span>
        ${port != null ? html`<span class="port">port ${port}</span>` : nothing}
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
      </div>
    </div>
    ${ui.openLogsFor === id ? html`<pre class="logs">${ui.logText || "(no output yet)"}</pre>` : nothing}
  `;
}

function projectSection(project: Project): TemplateResult {
  return html`
    <section class="group">
      <div class="group-head">
        <div class="titles">
          <h2>${project.name}</h2>
          <span class="root">${project.root}</span>
        </div>
        <div class="group-actions">
          <button
            title="Add command"
            @click=${() => void startAddCommand(project.id, project.root)}
          >
            <i class="ph ph-plus"></i> command
          </button>
        </div>
      </div>
      ${project.commands.length === 0
        ? html`<p class="empty-cmd">No commands. Add one.</p>`
        : project.commands.map((c) => commandRow(project, c))}
    </section>
  `;
}

function draw() {
  render(
    html`
      <header class="topbar">
        <h1><i class="ph ph-stack"></i> Server Supervisor</h1>
        <button class="add-project" @click=${() => void startAddProject()}>
          <i class="ph ph-folder-plus"></i> Add project
        </button>
      </header>
      ${ui.error ? html`<div class="error">${ui.error}</div>` : nothing}
      ${ui.projects.length === 0
        ? html`<p class="empty">No projects yet. Click "Add project" to pick a folder.</p>`
        : ui.projects.map(projectSection)}
      ${modalView()}
    `,
    ui.root,
  );
}
