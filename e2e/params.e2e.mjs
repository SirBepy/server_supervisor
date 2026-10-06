// Parameterized commands, end to end through the real webview + backend:
// kebab value picker (stopped and running), resolved text in rows, search and
// the log drawer, the modal's Parameters section, the suggestion table, and
// the "Combine commands..." migration. Run via e2e/run.mjs.
import { execFileSync } from "node:child_process";
import { api } from "./lib.mjs";

const PROJECT = "e2e-demo";

function cmdLineOf(pid) {
  return execFileSync(
    "powershell",
    ["-NoProfile", "-Command", `(Get-CimInstance Win32_Process -Filter "ProcessId=${pid}").CommandLine`],
    { encoding: "utf8" },
  ).trim();
}

async function proc(id) {
  const r = await api("GET", "/procs");
  return (r.json ?? []).find((p) => p.id === `${PROJECT}:${id}`);
}

async function waitFor(fn, what, ms = 15000) {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    const v = await fn();
    if (v) return v;
    await new Promise((r) => setTimeout(r, 300));
  }
  throw new Error(`timed out waiting for ${what}`);
}

function expect(cond, msg) {
  if (!cond) throw new Error(msg);
}

export async function run(page, step) {
  const card = (name) => page.locator(".card", { has: page.locator(".row-title", { hasText: new RegExp(`^${name}\\b`) }) });
  const menu = page.locator(".more-menu");
  const openCmdMenu = async (name) => {
    await page.keyboard.press("Escape");
    await card(name).click({ button: "right" });
    await menu.waitFor();
  };
  const closeMenus = () => page.keyboard.press("Escape");
  // Matches on text, not accessible name: Phosphor's icon-font glyph is part
  // of a button's accessible name, so a checked value never equals its label.
  const item = (label) => menu.locator("button").filter({ hasText: new RegExp(`^\\s*${label}\\s*$`) });
  // Row controls only render visibly while the row is hovered.
  const control = async (name, title) => {
    await card(name).hover();
    await card(name).getByTitle(title).click();
  };

  await step("project screen shows the resolved command, never braces", async () => {
    await page.locator(".proj-browse-row", { hasText: PROJECT }).click();
    const text = await card("pinger").locator(".row-cmdtext").textContent();
    expect(text.trim() === "ping -n 300 127.0.0.1", `row shows "${text}"`);
  });

  await step("kebab shows the param row while stopped", async () => {
    await openCmdMenu("pinger");
    await menu.getByRole("button", { name: "Count: Long" }).waitFor({ timeout: 3000 });
  });

  await step("param row swaps the menu to Back + values, current checked", async () => {
    await menu.getByRole("button", { name: "Count: Long" }).click();
    await menu.getByRole("button", { name: "Back" }).waitFor({ timeout: 3000 });
    const checked = await menu.locator("button:has(.ph-check)").textContent();
    expect(checked.trim() === "Long", `checked value is "${checked}"`);
  });

  await step("picking a value while stopped persists it and updates the row", async () => {
    await item("Longer").click();
    await waitFor(async () => (await card("pinger").locator(".row-cmdtext").textContent()).includes("-n 600"), "row to show -n 600");
    const p = await proc("pinger");
    expect(p.params[0].last_value === "longer", `last_value is ${p.params[0].last_value}`);
    expect(p.resolved_cmd === "ping -n 600 127.0.0.1", `resolved_cmd is ${p.resolved_cmd}`);
  });

  let firstPid;
  await step("Start spawns the remembered variant", async () => {
    await control("pinger", "Start");
    const p = await waitFor(async () => {
      const x = await proc("pinger");
      return x?.status === "running" && x.pid ? x : null;
    }, "pinger running");
    firstPid = p.pid;
    const line = cmdLineOf(p.pid);
    expect(line.includes("-n 600"), `spawned command line: ${line}`);
  });

  await step("kebab shows the param row while running too", async () => {
    await openCmdMenu("pinger");
    await menu.getByRole("button", { name: "Count: Longer" }).waitFor({ timeout: 3000 });
  });

  await step("picking a different value while running restarts into it", async () => {
    await menu.getByRole("button", { name: "Count: Longer" }).click();
    await item("Long").click();
    const p = await waitFor(async () => {
      const x = await proc("pinger");
      return x?.status === "running" && x.pid && x.pid !== firstPid ? x : null;
    }, "pinger restarted with a new pid");
    const line = cmdLineOf(p.pid);
    expect(line.includes("-n 300"), `restarted command line: ${line}`);
  });

  await step("re-picking the current value does not restart", async () => {
    const before = (await proc("pinger")).pid;
    await openCmdMenu("pinger");
    await menu.getByRole("button", { name: "Count: Long" }).click();
    await item("Long").click();
    await page.waitForTimeout(1500);
    const after = await proc("pinger");
    expect(after.pid === before, `pid changed ${before} -> ${after.pid}`);
    expect((await menu.count()) === 0, "menu stayed open");
  });

  await step("log drawer shows the resolved command line", async () => {
    await card("pinger").locator(".row-namecol").click();
    const line = await page.locator(".detail-cmdline").textContent();
    expect(line.includes("ping -n 300 127.0.0.1") && !line.includes("{"), `drawer shows "${line}"`);
    await card("pinger").locator(".row-namecol").click();
  });

  await step("Home running row shows resolved text and search matches it", async () => {
    await page.getByTitle("Back").click();
    const row = page.locator(".run-row .row-cmdtext").first();
    expect((await row.textContent()).trim() === "ping -n 300 127.0.0.1", `home row "${await row.textContent()}"`);
    await page.getByPlaceholder("Find a project...").fill("-n 300");
    await page.locator(".proj-browse-row", { hasText: PROJECT }).waitFor({ timeout: 3000 });
    await page.getByPlaceholder("Find a project...").fill("-n 999");
    await page.waitForTimeout(300);
    expect((await page.locator(".proj-browse-row", { hasText: PROJECT }).count()) === 0, "search for -n 999 still matched");
    await page.getByPlaceholder("Find a project...").fill("");
    await page.locator(".proj-browse-row", { hasText: PROJECT }).click();
  });

  await step("stop pinger", async () => {
    await control("pinger", "Stop");
    await waitFor(async () => (await proc("pinger"))?.status === "stopped", "pinger stopped");
  });

  await step("edit modal shows the Parameters section with the stored param", async () => {
    await openCmdMenu("pinger");
    await menu.getByRole("button", { name: /Edit command/ }).click();
    const block = page.locator(".dialog .param-block");
    await block.waitFor({ timeout: 3000 });
    expect((await block.locator(".param-name").inputValue()) === "count", "param name not prefilled");
    expect((await block.locator(".param-hint").textContent()).includes("{COUNT}"), "hint missing {COUNT}");
    expect((await block.locator(".param-value-row").count()) === 2, "expected 2 value rows");
  });

  await step("edit modal blocks a param whose token is missing from the cmd", async () => {
    const name = page.locator(".dialog .param-name");
    await name.fill("cnt");
    await page.locator(".dialog").getByRole("button", { name: "Save" }).click();
    const err = await page.locator(".dialog .field-error").textContent();
    expect(err.includes("{CNT}"), `inline error reads "${err}"`);
    await name.fill("count");
    expect((await page.locator(".dialog .field-error").count()) === 0, "stale error stayed after fixing the name");
  });

  await step("adding a value in the edit modal persists with a slugged id", async () => {
    await page.locator(".dialog").getByRole("button", { name: "Add value" }).click();
    const row = page.locator(".dialog .param-value-row").last();
    await row.locator(".param-value-label").fill("Short");
    await row.locator(".param-value-flag").fill("-n 100");
    await page.locator(".dialog").getByRole("button", { name: "Save" }).click();
    await page.locator(".dialog").waitFor({ state: "detached", timeout: 5000 });
    const p = await proc("pinger");
    const ids = p.params[0].values.map((v) => v.value).join(",");
    expect(ids === "long,longer,short", `value ids are ${ids}`);
  });

  await step("add-command modal offers stack suggestions and saves params", async () => {
    await page.getByTitle("More options").first().click();
    await menu.getByRole("button", { name: /Add command/ }).click();
    const dialog = page.locator(".dialog");
    await dialog.locator("input").first().waitFor();
    const cmdInput = dialog.getByPlaceholder(/npm run dev/).or(dialog.locator("input.combo-input")).first();
    await cmdInput.fill("flutter run {DEVICE}");
    await dialog.getByRole("button", { name: "Suggested parameter" }).click();
    const values = dialog.locator(".param-value-label");
    const labels = await values.evaluateAll((els) => els.map((e) => e.value));
    expect(labels.join(",") === "Chrome,Web Server,Android,iOS", `device suggestion labels ${labels}`);
    await dialog.getByRole("button", { name: "Suggested parameter" }).waitFor({ timeout: 2000 });
    await dialog.getByRole("button", { name: "Add", exact: true }).click();
    await dialog.waitFor({ state: "detached", timeout: 5000 });
    const procs = (await api("GET", "/procs")).json;
    const added = procs.find((p) => p.project === PROJECT && p.params?.[0]?.name === "device");
    expect(added, "no command with a device param was created");
    expect(added.resolved_cmd === "flutter run -d chrome", `resolved ${added.resolved_cmd}`);
  });

  await step("combine modal: running command disabled, diff prefilled, review lists differences", async () => {
    await control("plain", "Start");
    await waitFor(async () => (await proc("plain"))?.status === "running", "plain running");
    await page.getByTitle("More options").first().click();
    await menu.getByRole("button", { name: /Combine commands/ }).click();
    const dialog = page.locator(".dialog");
    const plainBox = dialog.locator("label", { hasText: "plain" }).locator("input[type=checkbox]");
    // The UI learns a proc is running on its next status poll, up to ~1s after the API does.
    await waitFor(() => plainBox.isDisabled(), "running command checkbox to disable", 4000);
    await dialog.locator("label", { hasText: "ping-a" }).locator("input[type=checkbox]").check();
    await dialog.locator("label", { hasText: "ping-b" }).locator("input[type=checkbox]").check();
    const template = await dialog.locator("input").evaluateAll((els) => els.map((e) => e.value).find((v) => v.includes("{")));
    expect(template === "ping -n {VARIANT} 127.0.0.1", `template prefill "${template}"`);
    const text = await dialog.textContent();
    expect(/role/i.test(text), "review does not list the differing role");
    expect(text.includes("ping-a (ping-a)") && text.includes("ping-b (ping-b)"), "review does not name retired ids");
  });

  await step("combine merges the pair into one templated command that runs each variant", async () => {
    const dialog = page.locator(".dialog");
    await dialog.getByRole("button", { name: /Combine 2 commands/ }).click();
    await dialog.waitFor({ state: "detached", timeout: 5000 });
    expect(!(await proc("ping-b")), "ping-b still exists");
    const merged = await waitFor(async () => {
      const procs = (await api("GET", "/procs")).json;
      return procs.find((p) => p.project === PROJECT && p.params?.[0]?.name === "variant");
    }, "merged command");
    expect(merged.resolved_cmd === "ping -n 400 127.0.0.1", `merged resolves to ${merged.resolved_cmd}`);
    expect(merged.name === "ping-a", `merged name ${merged.name}`);
    await control("ping-a", "Start");
    const p = await waitFor(async () => {
      const x = (await api("GET", "/procs")).json.find((q) => q.id === merged.id);
      return x?.status === "running" && x.pid ? x : null;
    }, "merged running");
    expect(cmdLineOf(p.pid).includes("-n 400"), "merged spawned the wrong variant");
    await openCmdMenu("ping-a");
    await menu.locator("button").filter({ hasText: /^\s*Variant: 400\s*$/ }).click();
    const labels = await menu.locator("button").allTextContents();
    expect(labels.some((l) => l.trim() === "500"), `value list ${labels}`);
    await closeMenus();
  });
}
