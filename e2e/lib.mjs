// Drives an isolated debug build of the app (its own identifier, so its own
// app-data dir, tray icon and single-instance lock) through WebView2's CDP
// port. The dev's installed supervisor and its servers are never touched.

import { spawn, execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";

export const E2E_DIR = path.dirname(fileURLToPath(import.meta.url));
export const REPO = path.dirname(E2E_DIR);
export const IDENTIFIER = "com.sirbepy.server-supervisor-e2e";
export const APP_DIR = path.join(process.env.APPDATA, IDENTIFIER);
export const DATA_DIR = path.join(APP_DIR, "supervisor");
export const CDP_PORT = 9333;
export const API_PORT = 6979;
const EXE = "D:/cargo-target/server_supervisor__src-tauri/debug/server_supervisor.exe";
const PID_FILE = path.join(E2E_DIR, ".app.pid");

export function screenshotDir() {
  const id = execFileSync(
    "powershell",
    ["-NoProfile", "-File", path.join(process.env.USERPROFILE, ".claude/skills/close/rename-session.ps1"), "-GetId"],
    { encoding: "utf8" },
  ).trim();
  const dir = path.join(REPO, ".for_bepy", "screenshots", id);
  fs.mkdirSync(dir, { recursive: true });
  return dir;
}

// Fresh state every run: the e2e app-data dir is owned by this harness alone.
export function seed(projects) {
  fs.rmSync(APP_DIR, { recursive: true, force: true });
  fs.mkdirSync(DATA_DIR, { recursive: true });
  fs.writeFileSync(
    path.join(APP_DIR, "settings.json"),
    JSON.stringify({
      api_port: API_PORT,
      __kit_auto_update: "never",
      __kit_theme: process.env.E2E_THEME ?? "light",
      keep_focus_on_launch: false,
    }),
  );
  fs.writeFileSync(path.join(DATA_DIR, "projects.json"), JSON.stringify(projects, null, 2));
}

export function launch() {
  const child = spawn(EXE, [], {
    detached: true,
    stdio: "ignore",
    env: { ...process.env, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${CDP_PORT}` },
  });
  child.unref();
  fs.writeFileSync(PID_FILE, String(child.pid));
  return child.pid;
}

export async function connect() {
  const deadline = Date.now() + 30000;
  let lastErr;
  while (Date.now() < deadline) {
    try {
      const browser = await chromium.connectOverCDP(`http://localhost:${CDP_PORT}`);
      const page = browser.contexts().flatMap((c) => c.pages())[0];
      if (page) {
        await page.waitForSelector(".topbar, .home, body", { timeout: 15000 });
        return { browser, page };
      }
    } catch (e) {
      lastErr = e;
    }
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error(`could not attach to the e2e app over CDP: ${lastErr}`);
}

export async function api(method, route, body) {
  const token = fs.readFileSync(path.join(DATA_DIR, "api_token.txt"), "utf8").trim();
  const r = await fetch(`http://127.0.0.1:${API_PORT}${route}`, {
    method,
    headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    body: body ? JSON.stringify(body) : undefined,
  });
  const text = await r.text();
  let json = null;
  try {
    json = JSON.parse(text);
  } catch {}
  return { status: r.status, json, text };
}

// Stops every supervised child first: the app deliberately leaves children
// running on exit, so killing it alone would orphan them.
export async function teardown() {
  try {
    const procs = await api("GET", "/procs");
    for (const p of procs.json ?? []) {
      if (p.status === "running" || p.status === "starting") await api("POST", `/procs/${p.id}/stop`);
    }
  } catch {}
  if (fs.existsSync(PID_FILE)) {
    const pid = fs.readFileSync(PID_FILE, "utf8").trim();
    try {
      execFileSync("taskkill", ["/F", "/T", "/PID", pid], { stdio: "ignore" });
    } catch {}
    fs.rmSync(PID_FILE, { force: true });
  }
}
