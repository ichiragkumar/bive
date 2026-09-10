// Shared helpers: escaping, ANSI rendering, state names, DOM lookup. No state.

/** getElementById or throw (dashboards must fail loud, never half-render). */
export function req<T extends HTMLElement = HTMLElement>(id: string): T {
  const el = document.getElementById(id);
  if (!el) throw new Error(`herdr UI: missing element #${id}`);
  return el as T;
}

export function esc(s: string): string {
  return s.replace(
    /[&<>"]/g,
    (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c] as string,
  );
}

/** Minimal ANSI → HTML for terminal rendering. */
export function ansiToHtml(text: string): string {
  const codes: Record<number, string> = { 30: "#6b7280", 31: "#e05656", 32: "#3fb96f", 33: "#e5b34a", 34: "#4c8dff", 35: "#c084fc", 36: "#22d3ee", 37: "#e6e9f0", 90: "#8b93a5" };
  let out = "";
  let color: string | null = null;
  for (const part of text.split(/\x1b\[/)) {
    const m = part.match(/^([\d;]+)m/);
    if (!m) {
      out += esc(part);
      continue;
    }
    const rest = part.slice(m[0].length);
    for (const code of m[1].split(";").map(Number)) {
      if (code === 0) color = null;
      else if (codes[code]) color = codes[code];
    }
    out += (color ? `<span style="color:${color}">` : "<span>") + esc(rest) + "</span>";
  }
  return out;
}

/** Daemon AgentState (string or {Variant: …}) → plain name. */
export function stateName(state: unknown): string {
  if (typeof state === "string") return state;
  if (typeof state === "object" && state !== null) {
    const obj = state as Record<string, unknown>;
    if ("Errored" in obj) return "Errored";
    if ("Exited" in obj) return "Exited";
    const keys = Object.keys(obj);
    if (keys.length) return keys[0];
  }
  return "Unknown";
}

/** Last visible line of a log tail, for list rows. */
export function lastLine(text: string): string {
  const clean = text.replace(/\x1b\[[?0-9;]*[a-zA-Z]/g, "");
  const lines = clean.trimEnd().split("\n");
  return lines[lines.length - 1] || "";
}

/** Tool-call markers, mirroring `is_tool_line` in ui_state.rs (Rust).
 *  Kept to the two claude-code markers + media placeholders on purpose:
 *  snapshot segments from Rust are authoritative; this only classifies
 *  live-appended lines between snapshots. */
export function isToolLine(line: string): boolean {
  const t = line.trimStart();
  return t.startsWith("⏺") || t.startsWith("⎿") || line.includes("[media:");
}
