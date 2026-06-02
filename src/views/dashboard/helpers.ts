// Pure string helpers shared by the dashboard view modules. No state, no IPC.

// Last path segment, ignoring trailing slashes.
export function basename(p: string): string {
  return p.replace(/[\\/]+$/, "").split(/[\\/]/).pop() ?? p;
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
