// WebView2 browser-process loss, end to end: force-kill the e2e app's own
// msedgewebview2 browser process (what a name-matched cleanup did to every
// com.sirbepy.* app on 2026-10-08) and expect the app to relaunch itself with
// a live dashboard while a supervised child keeps running. Runs last: it
// replaces the app process the other specs were driving. Run via e2e/run.mjs.
import { execFileSync } from "node:child_process";
import { api, adopt, connect, EXE, IDENTIFIER } from "./lib.mjs";

// tauri_kit_webview_recovery::RELAUNCH_GRACE, plus slack for the hook to land.
const GRACE_MS = 62000;

function psJson(script) {
  const out = execFileSync("powershell", ["-NoProfile", "-Command", `${script} | ConvertTo-Json -Compress`], {
    encoding: "utf8",
  }).trim();
  if (!out) return [];
  const v = JSON.parse(out);
  return Array.isArray(v) ? v : [v];
}

// The browser process is the one msedgewebview2 without a --type= flag. The
// trailing \EBWebView keeps this off the dev's real com.sirbepy.server-supervisor
// profile, whose name is a prefix of ours.
function browserPids() {
  const needle = `\\${IDENTIFIER}\\EBWebView`.replace(/\\/g, "\\\\");
  return psJson(
    `Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe'" | Where-Object { $_.CommandLine -match '${needle}' -and $_.CommandLine -notmatch '--type=' } | Select-Object -ExpandProperty ProcessId`,
  );
}

function appPids() {
  const exe = EXE.replace(/\//g, "\\");
  return psJson(
    `Get-CimInstance Win32_Process -Filter "Name='server_supervisor.exe'" | Where-Object { $_.ExecutablePath -eq '${exe}' } | Select-Object -ExpandProperty ProcessId`,
  );
}

async function waitFor(fn, what, ms) {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    // The relaunched app's API comes up a beat after its process does.
    const v = await Promise.resolve()
      .then(fn)
      .catch(() => null);
    if (v) return v;
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error(`timed out waiting for ${what}`);
}

function expect(cond, msg) {
  if (!cond) throw new Error(msg);
}

export async function run(step, { launchedAt, shots }) {
  const id = "e2e-demo:plain";
  let childPid;
  let oldApp;

  await step("a supervised child is running before the webview dies", async () => {
    await api("POST", `/procs/${id}/start`);
    const p = await waitFor(
      async () => (await api("GET", "/procs")).json?.find((x) => x.id === id && x.status === "running" && x.pid),
      "plain to run",
      15000,
    );
    childPid = p.pid;
    oldApp = appPids();
    expect(oldApp.length === 1, `expected one e2e app process, saw ${oldApp.length}`);
  });

  await step("killing the WebView2 browser process relaunches the app", async () => {
    const wait = launchedAt + GRACE_MS - Date.now();
    if (wait > 0) await new Promise((r) => setTimeout(r, wait));
    const browsers = browserPids();
    expect(browsers.length === 1, `expected one e2e browser process, saw ${browsers.length}`);
    execFileSync("taskkill", ["/F", "/PID", String(browsers[0])], { stdio: "ignore" });
    const fresh = await waitFor(
      () => {
        const now = appPids();
        return now.length === 1 && now[0] !== oldApp[0] ? now[0] : null;
      },
      "a relaunched app process",
      30000,
    );
    adopt(fresh);
  });

  await step("the relaunched dashboard renders", async () => {
    const { browser, page } = await connect();
    try {
      await page.evaluate(() => window.__TAURI__.window.getCurrentWindow().show());
      await page.locator(".proj-browse-row", { hasText: "e2e-demo" }).waitFor({ timeout: 15000 });
    } finally {
      await page.screenshot({ path: `${shots}/e2e-recovery-dashboard.png` }).catch(() => {});
      await browser.close();
    }
  });

  await step("the supervised child survived and was re-adopted", async () => {
    const p = await waitFor(
      async () => (await api("GET", "/procs")).json?.find((x) => x.id === id && x.status === "running"),
      "plain re-adopted as running",
      15000,
    );
    expect(p.pid === childPid, `child pid changed ${childPid} -> ${p.pid}`);
  });
}
