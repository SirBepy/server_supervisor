import { html, render, nothing, type TemplateResult } from "lit-html";
import { open } from "@tauri-apps/plugin-dialog";
import "./dashboard.css";
import * as ipc from "../../shared/ipc";
import type { ProcInfo, Project, DetectedCommand, ProcKind } from "../../types/ipc.generated";

const POLL_MS = 2500;

type PickedCommand = { name: string; cmd: string; kind: ProcKind };

type Modal =
  | null
  | {
      t: "addProject";
      name: string;
      root: string;
      detected: DetectedCommand[];
      picked: PickedCommand[];
      query: string;
      highlight: number;
      existingName: string | null;
    }
  | {
      t: "addCommand";
      projectId: string;
      root: string;
      detected: DetectedCommand[];
      name: string;
      cmd: string;
      kind: ProcKind;
      useDynamicPort: boolean;
      query: string;
      highlight: number;
    };

let root: HTMLElement;
let projects: Project[] = [];
let statusById: Record<string, ProcInfo> = {};
let openLogsFor: string | null = null;
let logText = "";
let error: string | null = null;
let modal: Modal = null;
// Whether the combobox dropdown is currently shown (driven by input focus).
let comboOpen = false;

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

// Normalize a folder path for equality: lowercase + strip trailing slash(es).
function normPath(p: string): string {
  return p.replace(/[\\/]+$/, "").toLowerCase();
}

// Find an existing project whose root matches the picked folder; return its name.
function existingProjectName(path: string): string | null {
  const want = normPath(path);
  return projects.find((p) => normPath(p.root) === want)?.name ?? null;
}

// Derive a short command name. `npm run X` / `pnpm run X` / `yarn X` -> X.
function deriveName(cmd: string): string {
  const c = cmd.trim();
  const m = c.match(/^(?:npm|pnpm)\s+run\s+(\S+)/i) ?? c.match(/^yarn\s+(\S+)/i);
  return m ? m[1] : c;
}

async function detectInto(path: string): Promise<DetectedCommand[]> {
  try {
    return await ipc.detectCommands(path);
  } catch (e) {
    error = String(e);
    return [];
  }
}

// Focus the name input once the modal has rendered, so picking a folder lands
// the cursor there immediately (folder name shows as a placeholder hint).
function focusNameField() {
  window.setTimeout(() => {
    root.querySelector<HTMLInputElement>(".dialog .field-row input")?.focus();
  }, 0);
}

async function startAddProject() {
  const picked = await open({ directory: true, multiple: false, title: "Pick a project folder" });
  if (typeof picked !== "string") return;
  const detected = await detectInto(picked);
  modal = {
    t: "addProject",
    name: "",
    root: picked,
    detected,
    picked: [],
    query: "",
    highlight: -1,
    existingName: existingProjectName(picked),
  };
  comboOpen = false;
  draw();
  focusNameField();
}

async function repickFolder() {
  if (modal?.t !== "addProject") return;
  const picked = await open({ directory: true, multiple: false, title: "Pick a project folder" });
  if (typeof picked !== "string") return;
  const detected = await detectInto(picked);
  modal = {
    t: "addProject",
    name: modal.name.trim(),
    root: picked,
    detected,
    picked: [],
    query: "",
    highlight: -1,
    existingName: existingProjectName(picked),
  };
  comboOpen = false;
  draw();
  focusNameField();
}

async function confirmAddProject() {
  if (modal?.t !== "addProject") return;
  const m = modal;
  if (!m.root.trim()) {
    error = "a folder is required";
    draw();
    return;
  }
  // Empty name falls back to the folder name (shown as the input's placeholder).
  const name = m.name.trim() || basename(m.root);
  try {
    const project = await ipc.addProject(name, m.root);
    for (const p of m.picked) {
      await ipc.addCommand(project.id, p.name, p.cmd, p.kind, false, false);
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
    await ipc.addCommand(m.projectId, m.name, m.cmd, m.kind, false, m.useDynamicPort);
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
  const port = statusById[id]?.port;
  return html`
    <div class="card">
      <div class="meta">
        ${dot(id)}
        <span class="name">${cmd.name}</span>
        ${cmd.kind === "flutter" ? html`<span class="tag">flutter</span>` : nothing}
        <span class="pid">${pid != null ? `pid ${pid}` : statusById[id]?.status ?? "stopped"}</span>
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
            @click=${async () => {
              const detected = await detectInto(project.root);
              modal = {
                t: "addCommand",
                projectId: project.id,
                root: project.root,
                detected,
                name: "",
                cmd: "",
                kind: "generic",
                useDynamicPort: false,
                query: "",
                highlight: -1,
              };
              comboOpen = false;
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

// ----- combobox -----

type ComboConfig = {
  query: string;
  highlight: number;
  suggestions: DetectedCommand[]; // already filtered + excluding picked
  showFreeText: boolean; // whether to render the "+ add ..." row as last item
  placeholder?: string;
  onQuery: (q: string) => void;
  onHighlight: (i: number) => void;
  onSelect: (d: DetectedCommand) => void;
  onFreeText: () => void;
};

// Total number of selectable rows (suggestions + optional free-text row).
function comboRowCount(c: ComboConfig): number {
  return c.suggestions.length + (c.showFreeText ? 1 : 0);
}

function comboBox(c: ComboConfig): TemplateResult {
  const rows = comboRowCount(c);
  const freeIdx = c.showFreeText ? c.suggestions.length : -1;

  const commit = (i: number) => {
    if (i < 0 || i >= rows) return;
    if (i === freeIdx) c.onFreeText();
    else c.onSelect(c.suggestions[i]);
  };

  const onKeydown = (e: KeyboardEvent) => {
    if (!comboOpen) {
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        comboOpen = true;
        draw();
        e.preventDefault();
        return;
      }
    }
    switch (e.key) {
      case "ArrowDown":
        if (rows === 0) return;
        e.preventDefault();
        c.onHighlight(c.highlight + 1 >= rows ? 0 : c.highlight + 1);
        break;
      case "ArrowUp":
        if (rows === 0) return;
        e.preventDefault();
        c.onHighlight(c.highlight - 1 < 0 ? rows - 1 : c.highlight - 1);
        break;
      case "Enter": {
        if (!comboOpen || rows === 0) return;
        e.preventDefault();
        // Default to the only-or-free-text row when nothing highlighted.
        const target = c.highlight >= 0 ? c.highlight : c.showFreeText ? freeIdx : 0;
        commit(target);
        break;
      }
      case "Escape":
        e.preventDefault();
        comboOpen = false;
        c.onQuery("");
        break;
    }
  };

  return html`
    <div class="combo">
      <input
        class="combo-input"
        placeholder=${c.placeholder ?? ""}
        .value=${c.query}
        @pointerdown=${() => {
          // Open on an explicit click, NOT on bare focus — otherwise returning
          // to the window refocuses the input and the same click lands on a row.
          comboOpen = true;
          draw();
        }}
        @blur=${() => {
          // Delay so a row's @click registers before close.
          window.setTimeout(() => {
            comboOpen = false;
            draw();
          }, 120);
        }}
        @input=${(e: Event) => {
          comboOpen = true;
          c.onQuery((e.target as HTMLInputElement).value);
        }}
        @keydown=${onKeydown}
      />
      ${comboOpen && rows > 0
        ? html`
            <div class="combo-pop" role="listbox">
              ${c.suggestions.map(
                (d, i) => html`
                  <div
                    class="combo-row ${c.highlight === i ? "active" : ""}"
                    role="option"
                    @mousedown=${(e: Event) => e.preventDefault()}
                    @mouseenter=${() => c.onHighlight(i)}
                    @click=${() => c.onSelect(d)}
                  >
                    <code>${d.cmd}</code>
                    <span class="combo-src">${d.source}</span>
                  </div>
                `,
              )}
              ${c.showFreeText
                ? html`
                    <div
                      class="combo-row free ${c.highlight === freeIdx ? "active" : ""}"
                      role="option"
                      @mousedown=${(e: Event) => e.preventDefault()}
                      @mouseenter=${() => c.onHighlight(freeIdx)}
                      @click=${() => c.onFreeText()}
                    >
                      + add "<code>${c.query.trim()}</code>"
                    </div>
                  `
                : nothing}
            </div>
          `
        : nothing}
    </div>
  `;
}

// Case-insensitive substring match against cmd AND name.
function filterDetected(detected: DetectedCommand[], query: string): DetectedCommand[] {
  const q = query.trim().toLowerCase();
  if (!q) return detected;
  return detected.filter(
    (d) => d.cmd.toLowerCase().includes(q) || d.name.toLowerCase().includes(q),
  );
}

function addProjectModal(m: Extract<Modal, { t: "addProject" }>): TemplateResult {
  const q = m.query.trim();
  const pickedCmds = new Set(m.picked.map((p) => p.cmd));
  const available = m.detected.filter((d) => !pickedCmds.has(d.cmd));
  const suggestions = filterDetected(available, m.query);
  // Free-text row when query is non-empty, not an exact existing suggestion cmd, and not already picked.
  const exactMatch = m.detected.some((d) => d.cmd === q) || pickedCmds.has(q);
  const showFreeText = q.length > 0 && !exactMatch;

  const addPicked = (p: PickedCommand) => {
    if (m.picked.some((x) => x.cmd === p.cmd)) return; // dedupe by cmd
    m.picked.push(p);
    m.query = "";
    m.highlight = -1;
    draw();
  };

  return html`
    <div class="overlay">
      <div class="dialog">
        <h3>Add project</h3>

        <div class="field-row">
          <label>Name</label>
          <input
            placeholder=${basename(m.root)}
            .value=${m.name}
            @input=${(e: Event) => (m.name = (e.target as HTMLInputElement).value)}
          />
        </div>
        <div class="field-row">
          <label>Folder</label>
          <span class="folder" title=${m.root}>${m.root}</span>
          <button class="ghost" tabindex="-1" @click=${() => void repickFolder()}>
            <i class="ph ph-folder-open"></i> Pick again
          </button>
        </div>

        ${m.existingName
          ? html`<p class="muted note">
              This folder is already "${m.existingName}" — new commands merge into it.
            </p>`
          : nothing}

        <div class="detect-head">
          <span>Commands</span>
        </div>

        ${m.picked.length > 0
          ? html`<div class="chips">
              ${m.picked.map(
                (p) => html`
                  <span class="chip">
                    <code>${p.cmd}</code>
                    <button
                      title="Remove"
                      tabindex="-1"
                      @click=${() => {
                        m.picked = m.picked.filter((x) => x.cmd !== p.cmd);
                        draw();
                      }}
                    >
                      <i class="ph ph-x"></i>
                    </button>
                  </span>
                `,
              )}
            </div>`
          : nothing}

        ${comboBox({
          query: m.query,
          highlight: m.highlight,
          suggestions,
          showFreeText,
          placeholder: "Search detected commands or type your own…",
          onQuery: (val) => {
            m.query = val;
            m.highlight = 0; // pre-highlight the top row so Enter takes it
            draw();
          },
          onHighlight: (i) => {
            m.highlight = i;
            draw();
          },
          onSelect: (d) => addPicked({ name: d.name, cmd: d.cmd, kind: d.kind }),
          onFreeText: () => {
            const cmd = m.query.trim();
            if (cmd) addPicked({ name: deriveName(cmd), cmd, kind: "generic" });
          },
        })}

        <div class="dialog-actions">
          <button @click=${closeModal}>Cancel</button>
          <button class="primary" @click=${() => void confirmAddProject()}>Add</button>
        </div>
      </div>
    </div>
  `;
}

function addCommandModal(m: Extract<Modal, { t: "addCommand" }>): TemplateResult {
  const q = m.query.trim();
  const suggestions = filterDetected(m.detected, m.query);
  const exactMatch = m.detected.some((d) => d.cmd === q);
  const showFreeText = q.length > 0 && !exactMatch;

  return html`
    <div class="overlay">
      <div class="dialog">
        <h3>Add command</h3>
        <div class="field-row">
          <label>Name</label>
          <input .value=${m.name} @input=${(e: Event) => (m.name = (e.target as HTMLInputElement).value)} />
        </div>
        <div class="field-row">
          <label>Command</label>
          ${comboBox({
            query: m.query,
            highlight: m.highlight,
            suggestions,
            showFreeText,
            placeholder: "npm run dev:up",
            onQuery: (val) => {
              m.query = val;
              m.cmd = val; // free-text: cmd tracks the typed text
              m.highlight = 0; // pre-highlight the top row so Enter takes it
              draw();
            },
            onHighlight: (i) => {
              m.highlight = i;
              draw();
            },
            onSelect: (d) => {
              m.cmd = d.cmd;
              m.name = d.name;
              m.kind = d.kind;
              m.query = d.cmd;
              m.highlight = -1;
              comboOpen = false;
              draw();
            },
            onFreeText: () => {
              const cmd = m.query.trim();
              m.cmd = cmd;
              if (!m.name.trim()) m.name = deriveName(cmd);
              m.highlight = -1;
              comboOpen = false;
              draw();
            },
          })}
        </div>
        <div class="field-row">
          <label>Kind</label>
          <select
            .value=${m.kind}
            @change=${(e: Event) => (m.kind = (e.target as HTMLSelectElement).value as ProcKind)}
          >
            <option value="generic">generic</option>
            <option value="flutter">flutter</option>
          </select>
        </div>
        <label class="detect-row">
          <input
            type="checkbox"
            .checked=${m.useDynamicPort}
            @change=${(e: Event) => (m.useDynamicPort = (e.target as HTMLInputElement).checked)}
          />
          <span>Assign a dynamic port</span>
        </label>
        <div class="dialog-actions">
          <button @click=${closeModal}>Cancel</button>
          <button class="primary" @click=${() => void confirmAddCommand()}>Add</button>
        </div>
      </div>
    </div>
  `;
}

function modalView(): TemplateResult | typeof nothing {
  if (!modal) return nothing;
  return modal.t === "addProject" ? addProjectModal(modal) : addCommandModal(modal);
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
