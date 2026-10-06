// `node e2e/run.mjs` - needs the e2e build first:
//   npx tauri build --debug --no-bundle --config e2e/tauri.e2e.conf.json
// Seeds a fresh isolated app, runs every spec step (a failing step is
// recorded and the run continues), screenshots each step, then tears the app
// and every child it started down.
import { seed, launch, connect, teardown, screenshotDir, E2E_DIR } from "./lib.mjs";
import { fixtureProjects } from "./fixture.mjs";
import { run as paramsSpec } from "./params.e2e.mjs";

const shots = screenshotDir();
let n = 0;
let failed = 0;

seed(fixtureProjects(E2E_DIR));
launch();
try {
  const { browser, page } = await connect();
  await page.evaluate(() => window.__TAURI__.window.getCurrentWindow().show());
  page.setDefaultTimeout(5000);
  const step = async (name, fn) => {
    n += 1;
    const tag = String(n).padStart(2, "0");
    try {
      await fn();
      console.log(`PASS ${tag} ${name}`);
    } catch (e) {
      failed += 1;
      console.log(`FAIL ${tag} ${name}\n     ${String(e.message ?? e).split("\n")[0]}`);
    }
    await page.screenshot({ path: `${shots}/e2e-${tag}.png` }).catch(() => {});
  };
  await paramsSpec(page, step);
  await browser.close();
} finally {
  await teardown();
}
console.log(`\n${n - failed}/${n} passed. Screenshots: ${shots}`);
process.exit(failed ? 1 : 0);
