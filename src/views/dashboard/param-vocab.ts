// Per-stack parameter suggestions for the add/edit-command modal's "+
// Suggested parameter" button. UI-only, never read by the backend: a
// `vocab.rs`-style lookup keyed by stack, pre-filling a starting point the
// dev can edit or remove freely (the free-form path is the same UI, not a
// separate mode).
//
// Stack detection reuses `techFromCmd` (helpers.ts) rather than a second
// program-token parser, so "what stack is this cmd" has one source of truth.

import { techFromCmd } from "./helpers";
import type { CommandParam } from "../../types/ipc.generated";

export type ParamValueSuggestion = { label: string; flag: string };
export type ParamSuggestion = { name: string; label: string; values: ParamValueSuggestion[] };

const FLUTTER_SUGGESTIONS: ParamSuggestion[] = [
  {
    name: "device",
    label: "Device",
    values: [
      { label: "Chrome", flag: "-d chrome" },
      { label: "Web Server", flag: "-d web-server" },
      { label: "Android", flag: "-d android" },
      { label: "iOS", flag: "-d ios" },
    ],
  },
  // Flavors are project-defined, so there's no sensible preset list - one
  // blank row is a starting point, not a guess.
  { name: "flavor", label: "Flavor", values: [{ label: "", flag: "" }] },
  {
    name: "dart-define-from-file",
    label: "Env file",
    values: [{ label: "", flag: "--dart-define-from-file=.env.dev" }],
  },
];

const NODE_SUGGESTIONS: ParamSuggestion[] = [
  { name: "script", label: "Script", values: [{ label: "", flag: "" }] },
  { name: "mode", label: "Mode", values: [{ label: "", flag: "" }] },
];

// Unused suggestions for this cmd text's detected stack ("unused" is filtered
// by the caller, which knows the modal's current params).
export function suggestedParamsForCmd(cmd: string): ParamSuggestion[] {
  const tech = techFromCmd(cmd);
  if (tech === "flutter") return FLUTTER_SUGGESTIONS;
  if (tech === "node") return NODE_SUGGESTIONS;
  return [];
}

// Turn a suggestion into a real `CommandParam`, slugging each preset value's
// id from its label up front (same rule the hand-authored path uses: slugged
// once, never re-derived). A suggestion with a blank starter label (flavor,
// script, mode) gets an empty `value` id, left for the lazy slug-on-first-
// label-input in params-field.ts, exactly like a hand-added blank value row.
export function suggestionToParam(s: ParamSuggestion): CommandParam {
  const used = new Set<string>();
  return {
    name: s.name,
    label: s.label,
    last_value: null,
    values: s.values.map((v) => ({
      value: v.label.trim() ? uniqueSlug(v.label, used) : "",
      label: v.label,
      flag: v.flag,
    })),
  };
}

// Lowercase, non-alphanumerics to "-", trimmed; empty input falls back to
// "value" so a blank label never produces an empty slug outright (callers
// still gate on the real emptiness check before this runs).
export function slugify(label: string): string {
  const slug = label
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
  return slug || "value";
}

// De-duplicates a slug against ids already used within the same param by
// appending a numeric suffix, mutating `used` so a caller slugging several
// values in one pass never collides with itself.
export function uniqueSlug(label: string, used: Set<string>): string {
  const base = slugify(label);
  let candidate = base;
  let n = 2;
  while (used.has(candidate)) {
    candidate = `${base}-${n}`;
    n++;
  }
  used.add(candidate);
  return candidate;
}
