// Detail Terminal tab: raw ANSI log pane. Ground truth when the Chat
// heuristic mis-groups.

import type { Store } from "../store.js";
import { ansiToHtml, req } from "./shared.js";

export function mountDetailTerminal(store: Store): void {
  const panel = req("tab-terminal");
  const logEl = req("detail-log");

  store.subscribe((s) => {
    const card = s.cards.find((c) => c.info.id === s.selectedId);
    if (!card || store.tabFor(card.info.id, card.info.profile) !== "terminal") return;
    logEl.innerHTML = ansiToHtml(card.log_tail || "");
    panel.scrollTop = panel.scrollHeight;
  });
}
