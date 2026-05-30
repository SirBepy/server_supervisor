// Minimal ANSI SGR -> colored lit-html renderer for the log viewer.
//
// Dev servers emit ANSI escape sequences (colors, bold, dim). Captured raw by
// the backend and rendered as plain text, they leak as literal `ESC[32m` noise.
// This walks a log string, turns SGR color/style runs into <span> segments, and
// strips every other escape sequence (cursor moves, OSC, etc.) so none of it
// shows. lit-html text bindings stay textContent, so this never injects HTML.

import { html, type TemplateResult } from "lit-html";

// 16-color palette tuned for the dark log background (#0a0c10): the standard 8
// at 30-37 / 40-47, the bright 8 at 90-97 / 100-107.
const STANDARD = [
  "#5c6370", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#abb2bf",
  "#7f848e", "#ff7b86", "#b5e890", "#f5d28b", "#7cc5ff", "#d68fee", "#6fd0dd", "#ffffff",
];

const FG: Record<number, string> = {};
const BG: Record<number, string> = {};
for (let i = 0; i < 8; i++) {
  FG[30 + i] = STANDARD[i];
  FG[90 + i] = STANDARD[8 + i];
  BG[40 + i] = STANDARD[i];
  BG[100 + i] = STANDARD[8 + i];
}

interface Style {
  fg?: string;
  bg?: string;
  bold: boolean;
  dim: boolean;
  italic: boolean;
  underline: boolean;
}

function fresh(): Style {
  return { bold: false, dim: false, italic: false, underline: false };
}

// xterm 256-color index -> css color.
function xterm256(n: number): string {
  if (n < 16) return STANDARD[n];
  if (n >= 232) {
    const v = 8 + (n - 232) * 10;
    return `rgb(${v},${v},${v})`;
  }
  const c = n - 16;
  const level = (x: number) => (x === 0 ? 0 : 55 + x * 40);
  return `rgb(${level(Math.floor(c / 36))},${level(Math.floor((c % 36) / 6))},${level(c % 6)})`;
}

// Apply one SGR sequence's numeric codes to the running style, in place.
function applyCodes(s: Style, codes: number[]): void {
  for (let i = 0; i < codes.length; i++) {
    const c = codes[i];
    if (c === 0) Object.assign(s, fresh());
    else if (c === 1) s.bold = true;
    else if (c === 2) s.dim = true;
    else if (c === 3) s.italic = true;
    else if (c === 4) s.underline = true;
    else if (c === 22) { s.bold = false; s.dim = false; }
    else if (c === 23) s.italic = false;
    else if (c === 24) s.underline = false;
    else if (c === 39) s.fg = undefined;
    else if (c === 49) s.bg = undefined;
    else if (FG[c]) s.fg = FG[c];
    else if (BG[c]) s.bg = BG[c];
    else if (c === 38 || c === 48) {
      // Extended color: `5;n` (256) or `2;r;g;b` (truecolor). Consume params.
      const mode = codes[i + 1];
      let color: string | undefined;
      if (mode === 5) { color = xterm256(codes[i + 2] ?? 0); i += 2; }
      else if (mode === 2) { color = `rgb(${codes[i + 2] ?? 0},${codes[i + 3] ?? 0},${codes[i + 4] ?? 0})`; i += 4; }
      if (color) { if (c === 38) s.fg = color; else s.bg = color; }
    }
  }
}

function toCss(s: Style): string {
  const parts: string[] = [];
  if (s.fg) parts.push(`color:${s.fg}`);
  if (s.bg) parts.push(`background:${s.bg}`);
  if (s.bold) parts.push("font-weight:600");
  if (s.dim) parts.push("opacity:0.6");
  if (s.italic) parts.push("font-style:italic");
  if (s.underline) parts.push("text-decoration:underline");
  return parts.join(";");
}

function span(text: string, s: Style): TemplateResult {
  const css = toCss(s);
  return css ? html`<span style=${css}>${text}</span>` : html`<span>${text}</span>`;
}

// Matches any escape sequence: CSI (incl. SGR), OSC, and lone two-char escapes.
// eslint-disable-next-line no-control-regex
const ANSI = /\x1b(?:\[[0-9;?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\)?|[@-Z\\-_])/g;
// A CSI that is specifically an SGR (color/style) sequence.
// eslint-disable-next-line no-control-regex
const SGR = /^\x1b\[[0-9;]*m$/;

/** Render a log string into colored lit-html spans, stripping non-SGR escapes. */
export function renderAnsi(text: string): TemplateResult {
  const segments: TemplateResult[] = [];
  const style = fresh();
  let last = 0;
  let m: RegExpExecArray | null;
  ANSI.lastIndex = 0;
  while ((m = ANSI.exec(text)) !== null) {
    if (m.index > last) segments.push(span(text.slice(last, m.index), style));
    if (SGR.test(m[0])) {
      const body = m[0].slice(2, -1); // drop leading ESC[ and trailing m
      const codes = body === "" ? [0] : body.split(";").map((n) => parseInt(n, 10) || 0);
      applyCodes(style, codes);
    }
    last = m.index + m[0].length;
  }
  if (last < text.length) segments.push(span(text.slice(last), style));
  return html`${segments}`;
}
