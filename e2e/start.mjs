// Seeds a fresh isolated app-data dir, launches the e2e build and waits until
// its webview is reachable over CDP. Leaves it running for the specs.
import { seed, launch, connect, E2E_DIR } from "./lib.mjs";
import { fixtureProjects } from "./fixture.mjs";

seed(fixtureProjects(E2E_DIR));
const pid = launch();
const { browser, page } = await connect();
await page.evaluate(() => window.__TAURI__.window.getCurrentWindow().show());
await page.setViewportSize({ width: 900, height: 900 }).catch(() => {});
console.log(`e2e app pid ${pid}, url ${page.url()}`);
await browser.close();
