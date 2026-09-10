// Detail Terminal tab: raw ANSI log pane, unchanged semantics from the old
// detail dialog. Ground truth when the Chat heuristic mis-groups.

import { ansiToHtml } from "./shared.js";

export function mountDetailTerminal(store) {
  const panel = document.getElementById("tab-terminal");
  const logEl = document.getElementById("detail-log");

  store.subscribe((s) => {
    const card = s.cards.find((c) => c.info.id === s.selectedId);
    if (!card || store.tabFor(card.info.id, card.info.profile) !== "terminal") return;
    logEl.innerHTML = ansiToHtml(card.log_tail || "");
    panel.scrollTop = panel.scrollHeight;
  });
}
