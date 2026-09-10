// Derived fleet views: every surface reads these, never raw state, so the
// sidebar, list, topbar, and palette always agree.

import type { AgentCard, SortKey, StoreState, SummaryCounts } from "./store.js";
import { stateName } from "./components/shared.js";

/** Attention rank for sorting: errored/blocked surface first. */
export function attentionRank(state: unknown): number {
  const n = stateName(state);
  if (n === "Errored") return 0;
  if (n === "Blocked") return 1;
  if (n === "Working" || n === "Starting") return 2;
  if (n === "Idle") return 3;
  return 4; // Exited and everything terminal sorts last.
}

export function summaryCounts(state: StoreState): SummaryCounts {
  const counts: SummaryCounts = {
    total: state.cards.length,
    working: 0,
    blocked: 0,
    errored: 0,
    idle: 0,
    exited: 0,
  };
  for (const c of state.cards) {
    const n = stateName(c.info.state);
    if (n === "Working" || n === "Starting") counts.working += 1;
    else if (n === "Blocked") counts.blocked += 1;
    else if (n === "Errored") counts.errored += 1;
    else if (n === "Idle") counts.idle += 1;
    else counts.exited += 1;
  }
  return counts;
}

/** Fleet rows for filter + host scope + search + sort. Cleared (hidden) ids
 *  are excluded. */
export function visibleAgents(state: StoreState): AgentCard[] {
  const q = state.search.trim().toLowerCase();
  const rows = state.cards.filter((c) => {
    if (state.hidden.includes(c.info.id)) return false;
    if (state.filter === "attention") {
      const n = stateName(c.info.state);
      if (n !== "Blocked" && n !== "Errored") return false;
    } else if (state.filter !== "all" && stateName(c.info.state) !== state.filter) {
      return false;
    }
    if (state.selectedHost !== null && (c.info.host || null) !== state.selectedHost)
      return false;
    if (q) {
      const hay =
        `${c.info.id} ${c.info.profile} ${c.info.command} ${c.info.cwd} ${c.info.host || "local"}`.toLowerCase();
      if (!hay.includes(q)) return false;
    }
    return true;
  });
  const by: Record<SortKey, (a: AgentCard, b: AgentCard) => number> = {
    attention: (a, b) =>
      attentionRank(a.info.state) - attentionRank(b.info.state) ||
      b.info.last_output_unix_ms - a.info.last_output_unix_ms,
    newest: (a, b) => b.info.started_at_unix_ms - a.info.started_at_unix_ms,
    activity: (a, b) => b.info.last_output_unix_ms - a.info.last_output_unix_ms,
    state: (a, b) =>
      stateName(a.info.state).localeCompare(stateName(b.info.state)) ||
      b.info.last_output_unix_ms - a.info.last_output_unix_ms,
  };
  return rows.sort(by[state.sort]);
}
