import { html, render, nothing, type TemplateResult } from "lit-html";
import "./dashboard.css";
import * as ipc from "../../shared/ipc";
import type { ProcInfo } from "../../types/ipc.generated";

const POLL_MS = 2500;

let root: HTMLElement;
let procs: ProcInfo[] = [];
let openLogsFor: string | null = null;
let logText = "";
let error: string | null = null;

export function mountDashboard(el: HTMLElement) {
  root = el;
  void refresh();
  window.setInterval(() => void refresh(), POLL_MS);
}

async function refresh() {
  try {
    procs = await ipc.listProcs();
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
    const lines = await ipc.getProcLogs(id);
    logText = lines.map((l) => l.text).join("\n");
  }
  draw();
}

function groupByProject(list: ProcInfo[]): Map<string, ProcInfo[]> {
  const map = new Map<string, ProcInfo[]>();
  for (const p of list) {
    const arr = map.get(p.project) ?? [];
    arr.push(p);
    map.set(p.project, arr);
  }
  return map;
}

function card(p: ProcInfo): TemplateResult {
  const running = p.status === "running";
  return html`
    <div class="card">
      <div class="meta">
        <span class="dot ${p.status}"></span>
        <span class="name">${p.name}</span>
        ${p.kind === "flutter" ? html`<span class="tag">flutter</span>` : nothing}
        <span class="pid">${p.pid != null ? `pid ${p.pid}` : p.status}</span>
      </div>
      <div class="actions">
        ${running
          ? html`
              <button title="Stop" @click=${() => act(ipc.stopProc(p.id))}>
                <i class="ph ph-stop"></i>
              </button>
              <button title="Restart" @click=${() => act(ipc.restartProc(p.id))}>
                <i class="ph ph-arrow-clockwise"></i>
              </button>
              ${p.kind === "flutter"
                ? html`<button title="Hot restart" @click=${() => act(ipc.reloadProc(p.id))}>
                    <i class="ph ph-arrows-clockwise"></i>
                  </button>`
                : nothing}
            `
          : html`
              <button title="Start" class="primary" @click=${() => act(ipc.startProc(p.id))}>
                <i class="ph ph-play"></i>
              </button>
            `}
        <button
          title="Logs"
          class=${openLogsFor === p.id ? "active" : ""}
          @click=${() => toggleLogs(p.id)}
        >
          <i class="ph ph-terminal-window"></i>
        </button>
      </div>
    </div>
  `;
}

function draw() {
  const groups = groupByProject(procs);
  render(
    html`
      <header class="topbar">
        <h1><i class="ph ph-stack"></i> Server Supervisor</h1>
      </header>
      ${error ? html`<div class="error">${error}</div>` : nothing}
      ${procs.length === 0
        ? html`<p class="empty">
            No processes declared. Add entries to <code>procs.json</code> in the supervisor
            data folder (see <code>procs.example.json</code>).
          </p>`
        : nothing}
      ${[...groups.entries()].map(
        ([project, items]) => html`
          <section class="group">
            <h2>${project}</h2>
            ${items.map(card)}
          </section>
        `,
      )}
      ${openLogsFor
        ? html`<pre class="logs">${logText || "(no output yet)"}</pre>`
        : nothing}
    `,
    root,
  );
}
