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
// any unrecognized command falls back to just its program name.
export function deriveName(cmd: string): string {
  const toks = cmd.trim().split(/\s+/).filter(Boolean);
  if (toks.length === 0) return cmd.trim();
  if (toks.includes("flutter")) return "flutter run";
  const [a, b, c] = toks;
  if ((a === "npm" || a === "pnpm" || a === "yarn" || a === "bun") && b === "run" && c) return c;
  if ((a === "yarn" || a === "pnpm" || a === "bun" || a === "npx") && b) return b;
  if (a === "cargo" && b) return `cargo ${b}`;
  return a;
}
