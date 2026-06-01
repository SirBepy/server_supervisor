// Pure string helpers shared by the dashboard view modules. No state, no IPC.

// Last path segment, ignoring trailing slashes.
export function basename(p: string): string {
  return p.replace(/[\\/]+$/, "").split(/[\\/]/).pop() ?? p;
}

// Normalize a folder path for equality: lowercase + strip trailing slash(es).
export function normPath(p: string): string {
  return p.replace(/[\\/]+$/, "").toLowerCase();
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
