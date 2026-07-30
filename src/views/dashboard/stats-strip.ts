import { html, nothing, type TemplateResult } from "lit-html";
import type { Project } from "../../types/ipc.generated";
import { ui, draw } from "./state";
import { formatBytes } from "./helpers";
import { openCommandInProject } from "./dashboard";
import { runningCount, projectIconTemplate } from "./home-screen";

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

export function statsStrip(): TemplateResult {
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
