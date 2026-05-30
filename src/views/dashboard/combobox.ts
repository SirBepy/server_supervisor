// Reusable combobox: a text input with a filterable dropdown of detected
// commands plus an optional "+ add free-text" row. Open/closed state lives in
// the shared `ui.comboOpen` flag; all selection behavior is driven by callbacks
// in ComboConfig so the combobox itself stays unaware of the modal it serves.

import { html, nothing, type TemplateResult } from "lit-html";
import type { DetectedCommand } from "../../types/ipc.generated";
import { ui, draw } from "./state";

export type ComboConfig = {
  query: string;
  highlight: number;
  suggestions: DetectedCommand[]; // already filtered + excluding picked
  showFreeText: boolean; // whether to render the "+ add ..." row as last item
  placeholder?: string;
  onQuery: (q: string) => void;
  onHighlight: (i: number) => void;
  onSelect: (d: DetectedCommand) => void;
  onFreeText: () => void;
};

// Total number of selectable rows (suggestions + optional free-text row).
export function comboRowCount(c: ComboConfig): number {
  return c.suggestions.length + (c.showFreeText ? 1 : 0);
}

export function comboBox(c: ComboConfig): TemplateResult {
  const rows = comboRowCount(c);
  const freeIdx = c.showFreeText ? c.suggestions.length : -1;

  const commit = (i: number) => {
    if (i < 0 || i >= rows) return;
    if (i === freeIdx) c.onFreeText();
    else c.onSelect(c.suggestions[i]);
  };

  const onKeydown = (e: KeyboardEvent) => {
    if (!ui.comboOpen) {
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        ui.comboOpen = true;
        draw();
        e.preventDefault();
        return;
      }
    }
    switch (e.key) {
      case "ArrowDown":
        if (rows === 0) return;
        e.preventDefault();
        c.onHighlight(c.highlight + 1 >= rows ? 0 : c.highlight + 1);
        break;
      case "ArrowUp":
        if (rows === 0) return;
        e.preventDefault();
        c.onHighlight(c.highlight - 1 < 0 ? rows - 1 : c.highlight - 1);
        break;
      case "Enter": {
        if (!ui.comboOpen || rows === 0) return;
        e.preventDefault();
        // Default to the only-or-free-text row when nothing highlighted.
        const target = c.highlight >= 0 ? c.highlight : c.showFreeText ? freeIdx : 0;
        commit(target);
        break;
      }
      case "Escape":
        e.preventDefault();
        ui.comboOpen = false;
        c.onQuery("");
        break;
    }
  };

  return html`
    <div class="combo">
      <input
        class="combo-input"
        placeholder=${c.placeholder ?? ""}
        .value=${c.query}
        @pointerdown=${() => {
          // Open on an explicit click, NOT on bare focus — otherwise returning
          // to the window refocuses the input and the same click lands on a row.
          ui.comboOpen = true;
          draw();
        }}
        @blur=${() => {
          // Delay so a row's @click registers before close.
          window.setTimeout(() => {
            ui.comboOpen = false;
            draw();
          }, 120);
        }}
        @input=${(e: Event) => {
          ui.comboOpen = true;
          c.onQuery((e.target as HTMLInputElement).value);
        }}
        @keydown=${onKeydown}
      />
      ${ui.comboOpen && rows > 0
        ? html`
            <div class="combo-pop" role="listbox">
              ${c.suggestions.map(
                (d, i) => html`
                  <div
                    class="combo-row ${c.highlight === i ? "active" : ""}"
                    role="option"
                    @mousedown=${(e: Event) => e.preventDefault()}
                    @mouseenter=${() => c.onHighlight(i)}
                    @click=${() => c.onSelect(d)}
                  >
                    <code>${d.cmd}</code>
                    <span class="combo-src">${d.source}</span>
                  </div>
                `,
              )}
              ${c.showFreeText
                ? html`
                    <div
                      class="combo-row free ${c.highlight === freeIdx ? "active" : ""}"
                      role="option"
                      @mousedown=${(e: Event) => e.preventDefault()}
                      @mouseenter=${() => c.onHighlight(freeIdx)}
                      @click=${() => c.onFreeText()}
                    >
                      + add "<code>${c.query.trim()}</code>"
                    </div>
                  `
                : nothing}
            </div>
          `
        : nothing}
    </div>
  `;
}

// Case-insensitive substring match against cmd AND name.
export function filterDetected(detected: DetectedCommand[], query: string): DetectedCommand[] {
  const q = query.trim().toLowerCase();
  if (!q) return detected;
  return detected.filter(
    (d) => d.cmd.toLowerCase().includes(q) || d.name.toLowerCase().includes(q),
  );
}
