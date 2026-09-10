// Agent list: selection-driven rows (replaces the card grid; a list scales
// past ~12 agents where a grid doesn't). Keyboard: j/k or arrows move,
// Enter jumps to the composer.

import type { Store } from "../store.js";
import { esc, lastLine, req, stateName } from "./shared.js";
import type { AgentCard } from "../store.js";

export function mountAgentList(store: Store, onSelect: (id: string) => void): void {
  const list = req("agent-list");

  /** Rows for the current filter + sidebar host scope. */
  function visibleInScope(): AgentCard[] {
    const s = store.state;
    return s.cards.filter(
      (c) =>
        (s.filter === "all" || stateName(c.info.state) === s.filter) &&
        (s.selectedHost === null || (c.info.host || null) === s.selectedHost),
    );
  }

  store.subscribe((s) => renderList(visibleInScope(), s.selectedId));

  function renderList(visible: AgentCard[], selectedId: string | null): void {
    list.innerHTML = "";
    if (!visible.length) {
      const li = document.createElement("li");
      li.className = "empty-row";
      li.textContent = "No agents. Spawn one from the sidebar or run herdr spawn.";
      list.appendChild(li);
      return;
    }
    for (const card of visible) {
      const li = document.createElement("li");
      li.className = "agent-row";
      if (card.info.id === selectedId) li.classList.add("selected");
      const st = stateName(card.info.state);
      li.innerHTML =
        `<span class="id">${esc(card.info.id.slice(0, 12))}</span>` +
        `<span class="profile">${esc(card.info.profile)}</span>` +
        `<span class="badge ${esc(st)}">${esc(st)}</span>` +
        `<span class="last-line">${esc(lastLine(card.log_tail || "")) || "&nbsp;"}</span>`;
      li.onclick = () => onSelect(card.info.id);
      list.appendChild(li);
    }
  }

  document.addEventListener("keydown", (e: KeyboardEvent) => {
    if (/INPUT|SELECT|TEXTAREA/.test(document.activeElement?.tagName ?? "")) return;
    const visible = visibleInScope();
    if (!visible.length) return;
    let idx = visible.findIndex((c) => c.info.id === store.state.selectedId);
    const move = (d: number) => {
      idx = Math.min(visible.length - 1, Math.max(0, (idx < 0 ? 0 : idx) + d));
      onSelect(visible[idx].info.id);
    };
    if (e.key === "j" || e.key === "ArrowDown") move(1);
    else if (e.key === "k" || e.key === "ArrowUp") move(-1);
    else if (e.key === "Enter") req("composer").focus();
  });
}
