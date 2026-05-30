// Pure string helpers shared by the dashboard view modules. No state, no IPC.

// Last path segment, ignoring trailing slashes.
export function basename(p: string): string {
  return p.replace(/[\\/]+$/, "").split(/[\\/]/).pop() ?? p;
}

// Normalize a folder path for equality: lowercase + strip trailing slash(es).
export function normPath(p: string): string {
  return p.replace(/[\\/]+$/, "").toLowerCase();
}

// Derive a short command name. `npm run X` / `pnpm run X` / `yarn X` -> X.
export function deriveName(cmd: string): string {
  const c = cmd.trim();
  const m = c.match(/^(?:npm|pnpm)\s+run\s+(\S+)/i) ?? c.match(/^yarn\s+(\S+)/i);
  return m ? m[1] : c;
}
