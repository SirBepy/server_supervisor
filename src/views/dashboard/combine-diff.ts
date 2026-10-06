// Pure diff helpers for the "Combine into one command" modal's pre-fill: a
// typing shortcut only, never silently applied (the modal keeps every field
// editable after this runs once). No state, no IPC - easy to reason about
// and the only piece of this feature with no test runner to exercise it in.

// Longest common whitespace-token PREFIX and SUFFIX shared by every cmd in
// `cmds`, capped so the two spans never overlap (bounded by the shortest
// cmd's token count). `middles` is each cmd's remaining tokens, in order -
// the per-command differing span the modal offers as that row's flag.
export function diffCommandTokens(cmds: string[]): { prefix: string[]; suffix: string[]; middles: string[][] } {
  const tokLists = cmds.map((c) => c.trim().split(/\s+/).filter(Boolean));
  if (tokLists.length === 0) return { prefix: [], suffix: [], middles: [] };
  const minLen = Math.min(...tokLists.map((t) => t.length));
  let prefixLen = 0;
  while (prefixLen < minLen && tokLists.every((t) => t[prefixLen] === tokLists[0][prefixLen])) {
    prefixLen++;
  }
  let suffixLen = 0;
  while (
    suffixLen < minLen - prefixLen &&
    tokLists.every((t) => t[t.length - 1 - suffixLen] === tokLists[0][tokLists[0].length - 1 - suffixLen])
  ) {
    suffixLen++;
  }
  const prefix = tokLists[0].slice(0, prefixLen);
  const suffix = suffixLen > 0 ? tokLists[0].slice(tokLists[0].length - suffixLen) : [];
  const middles = tokLists.map((t) => t.slice(prefixLen, t.length - suffixLen));
  return { prefix, suffix, middles };
}

// `{AXIS}` placeholder spliced between the shared prefix/suffix. If the
// ticked commands share neither (a genuinely unrelated pair), this reads as
// visibly odd in the modal before confirm - intended, not a bug to guard
// against (see the migration spec's "Out of scope" notes).
export function templateFromTokens(prefix: string[], suffix: string[], axisName: string): string {
  const token = `{${(axisName.trim() || "AXIS").toUpperCase()}}`;
  return [...prefix, token, ...suffix].join(" ");
}

// Each ticked command's differing span, space-joined back into one flag.
// May be empty (that source becomes the "no flag" value).
export function flagsFromMiddles(middles: string[][]): string[] {
  return middles.map((m) => m.join(" "));
}

// A value row's label: its flag text, or the source command's own name when
// the flag is empty (an empty flag reads as a blank label otherwise).
export function labelsFromFlags(flags: string[], names: string[]): string[] {
  return flags.map((f, i) => f.trim() || names[i]);
}

// Tiny axis-name suggestion from the differing spans - never anything
// resembling clustering/similarity, just a literal substring check on text
// the dev is about to see and can freely overwrite.
export function guessAxisName(flags: string[]): string {
  const nonEmpty = flags.map((f) => f.trim()).filter(Boolean);
  if (nonEmpty.length === 0) return "variant";
  if (nonEmpty.every((f) => f.startsWith("-d "))) return "device";
  if (nonEmpty.some((f) => f.includes("--dart-define-from-file"))) return "env";
  if (nonEmpty.some((f) => f.includes("--flavor"))) return "flavor";
  return "variant";
}
