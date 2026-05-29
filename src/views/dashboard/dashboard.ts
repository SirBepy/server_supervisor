import { html, render, nothing, type TemplateResult } from "lit-html";
import { open } from "@tauri-apps/plugin-dialog";
import "./dashboard.css";
import * as ipc from "../../shared/ipc";
import type { ProcInfo, Project, DetectedCommand, ProcKind } from "../../types/ipc.generated";

const POLL_MS = 2500;

type Modal =
  | null
  | {
      t: "addProject";
      name: string;
      root: string;
      detected: DetectedCommand[];
      selected: Set<number>;
    }
  | { t: "addCommand"; projectId: string; name: string; cmd: string; kind: ProcKind };

let root: HTMLElement;
let projects: Project[] = [];
let statusById: Record<string, ProcInfo> = {};
let openLogsFor: string | null = null;
let logText = "";
let error: string | null = null;
let modal: Modal = null;

export function mountDashboard(el: HTMLElement) {
  root = el;
  void refresh();
  window.setInterval(() => void refresh(), POLL_MS);
}

async function refresh() {
  try {
    const [projs, procs] = await Promise.all([ipc.listProjects(), ipc.listProcs()]);
    projects = projs;
    statusById = Object.fromEntries(procs.map((p) => [p.id, p]));
    error = null;
    if (openLogsFor) {
      const lines = await ipc.getProcLogs(openLogsFor);
      logText = lines.map((l) => l.text).join("\n");
    }
  } catch (e) {
    error = String(e);
  }
  draw();
}

async function act(p: Promise<unknown>) {
  try {
    await p;
    error = null;
  } catch (e) {
    error = String(e);
  }
  await refresh();
}

async function toggleLogs(id: string) {
  if (openLogsFor === id) {
    openLogsFor = null;
    logText = "";
  } else {
    openLogsFor = id;
    logText = (await ipc.getProcLogs(id)).map((l) => l.text).join("\n");
  }
  draw();
}

// ----- add-project wizard -----

function basename(p: string): string {
  return p.replace(/[\\/]+$/, "").split(/[\\/]/).pop() ?? p;
}

async function startAddProject() {
  const picked = await open({ directory: true, multiple: false, title: "Pick a project folder" });
  if (typeof picked !== "string") return;
  let detected: DetectedCommand[] = [];
  try {
    detected = await ipc.detectCommands(picked);
  } catch (e) {
    error = String(e);
  }
  modal = {
    t: "addProject",
    name: basename(picked),
    root: picked,
    detected,
    // Pre-select package.json + launch.json finds; leave fuzzy readme unchecked.
    selected: new Set(detected.flatMap((d, i) => (d.source === "readme" ? [] : [i]))),
  };
  draw();
}

async function confirmAddProject() {
  if (modal?.t !== "addProject") return;
  const m = modal;
  if (!m.name.trim() || !m.root.trim()) {
    error = "name and folder are required";
    draw();
    return;
  }
  try {
    const project = await ipc.addProject(m.name, m.root);
    for (const i of m.selected) {
      const d = m.detected[i];
      await ipc.addCommand(project.id, d.name, d.cmd, d.kind, false);
    }
    error = null;
    modal = null;
  } catch (e) {
    error = String(e);
  }
  await refresh();
}

async function confirmAddCommand() {
  if (modal?.t !== "addCommand") return;
  const m = modal;
  try {
    await ipc.addCommand(m.projectId, m.name, m.cmd, m.kind, false);
    error = null;
    modal = null;
  } catch (e) {
    error = String(e);
  }
  await refresh();
}

function closeModal() {
  modal = null;
  draw();
}

// ----- rendering -----

function dot(id: string): TemplateResult {
  const status = statusById[id]?.status ?? "stopped";
  return html`<span class="dot ${status}" title=${status}></span>`;
}

function commandRow(project: Project, cmd: Project["commands"][number]): TemplateResult {
  const id = `${project.id}:${cmd.id}`;
  const running = statusById[id]?.status === "running";
  const pid = statusById[id]?.pid;
  return html`
    <div class="card">
      <div class="meta">
        ${dot(id)}
        <span class="name">${cmd.name}</span>
        ${cmd.kind === "flutter" ? html`<span class="tag">flutter</span>` : nothing}
        <span class="pid">${pid != null ? `pid ${pid}` : statusById[id]?.status ?? "stopped"}</span>
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
          class=${openLogsFor === id ? "active" : ""}
          @click=${() => toggleLogs(id)}
        >
          <i class="ph ph-terminal-window"></i>
        </button>
        <button title="Remove command" @click=${() => act(ipc.removeCommand(project.id, cmd.id))}>
          <i class="ph ph-trash"></i>
        </button>
      </div>
    </div>
    ${openLogsFor === id ? html`<pre class="logs">${logText || "(no output yet)"}</pre>` : nothing}
  `;
}

function projectSection(project: Project): TemplateResult {
  return html`
    <section class="group">
      <div class="group-head">
        <div>
          <h2>${project.name}</h2>
          <span class="root" title=${project.root}>${project.root}</span>
        </div>
        <div class="group-actions">
          <button
            title="Add command"
            @click=${() => {
              modal = { t: "addCommand", projectId: project.id, name: "", cmd: "", kind: "generic" };
              draw();
            }}
          >
            <i class="ph ph-plus"></i> command
          </button>
          <button title="Remove project" @click=${() => act(ipc.removeProject(project.id))}>
            <i class="ph ph-trash"></i>
          </button>
        </div>
      </div>
      ${project.commands.length === 0
        ? html`<p class="empty-cmd">No commands. Add one.</p>`
        : project.commands.map((c) => commandRow(project, c))}
    </section>
  `;
}

function modalView(): TemplateResult | typeof nothing {
  if (!modal) return nothing;
  if (modal.t === "addProject") {
    const m = modal;
    return html`
      <div class="overlay" @click=${(e: Event) => e.target === e.currentTarget && closeModal()}>
        <div class="dialog">
          <h3>Add project</h3>
          <label>Name</label>
          <input
            .value=${m.name}
            @input=${(e: Event) => (m.name = (e.target as HTMLInputElement).value)}
          />
          <label>Folder</label>
          <div class="rootline">${m.root}</div>
          <label>Detected commands</label>
          ${m.detected.length === 0
            ? html`<p class="muted">None found. Create the project, then add commands manually.</p>`
            : html`<div class="detect-list">
                ${m.detected.map(
                  (d, i) => html`
                    <label class="detect-item">
                      <input
                        type="checkbox"
                        .checked=${m.selected.has(i)}
                        @change=${(e: Event) => {
                          (e.target as HTMLInputElement).checked
                            ? m.selected.add(i)
                            : m.selected.delete(i);
                        }}
                      />
                      <span class="d-name">${d.name}</span>
                      <code>${d.cmd}</code>
                      <span class="d-src">${d.source}</span>
                    </label>
                  `,
                )}
              </div>`}
          <div class="dialog-actions">
            <button @click=${closeModal}>Cancel</button>
            <button class="primary" @click=${() => void confirmAddProject()}>Add</button>
          </div>
        </div>
      </div>
    `;
  }
  // addCommand
  const m = modal;
  return html`
    <div class="overlay" @click=${(e: Event) => e.target === e.currentTarget && closeModal()}>
      <div class="dialog">
        <h3>Add command</h3>
        <label>Name</label>
        <input .value=${m.name} @input=${(e: Event) => (m.name = (e.target as HTMLInputElement).value)} />
        <label>Command (run via cmd /C)</label>
        <input
          placeholder="npm run dev:up"
          .value=${m.cmd}
          @input=${(e: Event) => (m.cmd = (e.target as HTMLInputElement).value)}
        />
        <label>Kind</label>
        <select @change=${(e: Event) => (m.kind = (e.target as HTMLSelectElement).value as ProcKind)}>
          <option value="generic" ?selected=${m.kind === "generic"}>generic</option>
          <option value="flutter" ?selected=${m.kind === "flutter"}>flutter</option>
        </select>
        <div class="dialog-actions">
          <button @click=${closeModal}>Cancel</button>
          <button class="primary" @click=${() => void confirmAddCommand()}>Add</button>
        </div>
      </div>
    </div>
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
      ${error ? html`<div class="error">${error}</div>` : nothing}
      ${projects.length === 0
        ? html`<p class="empty">No projects yet. Click "Add project" to pick a folder.</p>`
        : projects.map(projectSection)}
      ${modalView()}
    `,
    root,
  );
}
