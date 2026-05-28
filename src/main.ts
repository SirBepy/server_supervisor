import "@phosphor-icons/web";
import "./styles/base.css";
import { html, render } from "lit-html";

const app = document.getElementById("app")!;

render(
  html`
    <main class="shell">
      <h1><i class="ph ph-stack"></i> Server Supervisor</h1>
      <p class="muted">
        Phase 1 skeleton is live. Closing this window hides to the tray; the
        process stays running. Tray "Quit" exits and (once wired) kills every
        child it started.
      </p>
      <p class="muted">
        Next phases: the process registry, the localhost API, the Flutter
        adapter, and this dashboard.
      </p>
    </main>
  `,
  app,
);
