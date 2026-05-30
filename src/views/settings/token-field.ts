import { html } from "lit-html";
import type { CustomField } from "../../../vendor/tauri_kit/frontend/settings/schema";

export function makeTokenField(token: string): CustomField {
  return {
    // Double-underscore prefix keeps this key out of the Rust Settings struct.
    // Serde's default behaviour (no deny_unknown_fields) silently drops unknown
    // keys on load, so a spurious save round-trip is harmless.
    key: "__api_token_display",
    kind: "custom",
    label: "API token",
    tooltip: "Bearer token for the localhost HTTP API. Paste this into your AI agent config.",
    // `value` is always undefined here: the kit passes current["__api_token_display"]
    // which is never written to settings. The closure `token` is the correct source —
    // the token file is written once at startup and is stable for the session lifetime.
    render(_value: unknown, _onChange: (next: unknown) => void) {
      return html`
        <label class="kit-row">
          <span class="kit-row-label">API token</span>
          <span class="token-row">
            <input class="token-input" readonly .value=${token} />
            <button
              class="token-copy"
              title="Copy token"
              @click=${() => void navigator.clipboard.writeText(token)}
            >
              <i class="ph ph-copy"></i>
            </button>
          </span>
        </label>
      `;
    },
  };
}
