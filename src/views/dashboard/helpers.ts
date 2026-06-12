// Pure string helpers shared by the dashboard view modules. No state, no IPC.

import type { Project, ProcInfo } from "../../types/ipc.generated";

// Last path segment, ignoring trailing slashes.
export function basename(p: string): string {
  return p.replace(/[\\/]+$/, "").split(/[\\/]/).pop() ?? p;
}

// Default display name for a project folder (mirrors the backend
// `smart_project_name`). A folder literally named "app" or "src" is a useless
// label, so prefix it with its parent folder: ".../myproject/app" -> "myproject-app".
// Any other folder just uses its own basename.
export function smartProjectName(p: string): string {
  const segs = p.replace(/[\\/]+$/, "").split(/[\\/]/).filter(Boolean);
  const folder = segs[segs.length - 1] ?? p;
  const low = folder.toLowerCase();
  if ((low === "app" || low === "src") && segs.length >= 2) {
    return `${segs[segs.length - 2]}-${folder}`;
  }
  return folder;
}

// Normalize a folder path for equality: lowercase + strip trailing slash(es).
export function normPath(p: string): string {
  return p.replace(/[\\/]+$/, "").toLowerCase();
}

// Human-readable resident memory for the RAM badge: MB below ~1 GB (one decimal
// under 10 MB, whole numbers above), then GB with two decimals. Returns "-" for
// null/undefined so a stopped process shows no live-looking "0".
export function formatBytes(bytes: number | bigint | null | undefined): string {
  if (bytes == null) return "-";
  // mem_bytes is a Rust u64, which ts-rs emits as bigint. A few GB is well within
  // Number's safe range, so widen to Number for the arithmetic.
  const mb = Number(bytes) / (1024 * 1024);
  if (mb < 1024) return `${mb < 10 ? mb.toFixed(1) : Math.round(mb)} MB`;
  return `${(mb / 1024).toFixed(2)} GB`;
}

// Human uptime for the expanded-card "started N ago" line, from a unix-ms start
// timestamp. Returns "" for null so the caller can omit the clause entirely.
export function formatUptime(startedAtMs: number | bigint | null | undefined): string {
  if (startedAtMs == null) return "";
  const sec = Math.floor((Date.now() - Number(startedAtMs)) / 1000);
  if (sec < 60) return "just now";
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min} minute${min === 1 ? "" : "s"} ago`;
  const hr = Math.floor(min / 60);
  if (hr < 24) return `${hr} hour${hr === 1 ? "" : "s"} ago`;
  const day = Math.floor(hr / 24);
  return `${day} day${day === 1 ? "" : "s"} ago`;
}

// Derive a short command name (mirrors the backend `derive_name`). Never returns
// the whole command line: a long Flutter launch collapses to "flutter run", and
// any unrecognized command falls back to its program basename (no path, no ext).
export function deriveName(cmd: string): string {
  const toks = cmd.trim().split(/\s+/).filter(Boolean);
  if (toks.length === 0) return cmd.trim();
  const [a, b, c] = toks;
  const prog = basename(a).replace(/\.(exe|cmd|bat|sh)$/i, "");
  if (prog === "flutter" || toks.includes("flutter")) return "flutter run";
  if ((prog === "npm" || prog === "pnpm" || prog === "yarn" || prog === "bun") && b === "run" && c) return c;
  if ((prog === "yarn" || prog === "pnpm" || prog === "bun" || prog === "npx") && b) return b;
  if (prog === "cargo" && b) return `cargo ${b}`;
  return toks.length === 1 ? prog : `${prog} ${b}`;
}

const MAX_DISPLAY_NAME = 38;

// Display name for a proc: keeps short clean stored names as-is, re-derives
// from the command for long or path-like stored names, then truncates.
export function displayName(spec: { name: string; cmd: string }): string {
  const n = spec.name;
  if (n.length <= MAX_DISPLAY_NAME && !/[/\\]/.test(n)) return n;
  const d = deriveName(spec.cmd);
  return d.length <= MAX_DISPLAY_NAME ? d : d.slice(0, MAX_DISPLAY_NAME - 1) + "…";
}

// Tech identities we can draw a Devicon logo for.
export type TechKey =
  | "rust" | "flutter" | "node" | "python" | "go" | "deno" | "docker" | "dotnet";

// Program basename (lowercased, extension-stripped) -> tech. Mirrors deriveName's
// program parsing so the row logo matches how commands are actually launched.
const TECH_BY_PROG: Record<string, TechKey> = {
  cargo: "rust", rustc: "rust", rustup: "rust",
  flutter: "flutter", dart: "flutter",
  npm: "node", pnpm: "node", yarn: "node", bun: "node", npx: "node", node: "node",
  python: "python", python3: "python", py: "python",
  go: "go",
  deno: "deno",
  docker: "docker", "docker-compose": "docker",
  dotnet: "dotnet",
};

// Detect the tech of a single command string from its program token. Handles the
// fvm wrapper Joe uses for Flutter ("fvm flutter run").
export function techFromCmd(cmd: string): TechKey | null {
  const toks = cmd.trim().split(/\s+/).filter(Boolean);
  if (toks.length === 0) return null;
  const prog = basename(toks[0]).replace(/\.(exe|cmd|bat|sh|ps1)$/i, "").toLowerCase();
  if (prog === "fvm" && toks[1] && basename(toks[1]).toLowerCase().startsWith("flutter")) {
    return "flutter";
  }
  return TECH_BY_PROG[prog] ?? null;
}

// A project's tech: prefer a currently-live command's tech (that's what the user
// cares about right now), else the first command with a detectable tech.
export function projectTech(
  project: Project,
  statusById: Record<string, ProcInfo>,
): TechKey | null {
  const isLive = (c: Project["commands"][number]) => {
    const st = statusById[`${project.id}:${c.id}`]?.status;
    return st === "running" || st === "starting";
  };
  const live = project.commands.find(isLive);
  const ordered = live ? [live, ...project.commands] : project.commands;
  for (const c of ordered) {
    const t = techFromCmd(c.cmd);
    if (t) return t;
  }
  return null;
}

// Devicon class per tech. `colored` is appended where the brand has a colored
// variant; rust has none (tinted via CSS instead). Class names verified against
// the installed devicon.min.css (deno is `denojs`, .NET is `dotnetcore`, Go's
// colored mark is `go-original-wordmark`).
const DEVICON: Record<TechKey, string> = {
  rust: "devicon-rust-plain",
  flutter: "devicon-flutter-plain colored",
  node: "devicon-nodejs-plain colored",
  python: "devicon-python-plain colored",
  go: "devicon-go-original-wordmark colored",
  deno: "devicon-denojs-original",
  docker: "devicon-docker-plain colored",
  dotnet: "devicon-dotnetcore-plain colored",
};

export function deviconClass(tech: TechKey): string {
  return DEVICON[tech];
}
