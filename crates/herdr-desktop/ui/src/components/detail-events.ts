// Detail Events tab: structured client-side timeline (spawn, output bursts,
// state changes, media, exits, sends). Debugging view over the event stream.

import type { Store, TimelineEntry } from "../store.js";
import { req } from "./shared.js";

export function mountDetailEvents(store: Store): void {
  const list = req("events-timeline");

  store.subscribe((s) => {
    const card = s.cards.find((c) => c.info.id === s.selectedId);
    if (!card || store.tabFor(card.info.id, card.info.profile) !== "events") return;
    renderTimeline(s.timeline[card.info.id] || []);
  });

  function renderTimeline(rows: TimelineEntry[]): void {
    list.innerHTML = "";
    if (!rows.length) {
      const li = document.createElement("li");
      li.className = "muted";
      li.textContent = "No events yet for this agent.";
      list.appendChild(li);
      return;
    }
    for (const r of [...rows].reverse()) {
      const li = document.createElement("li");
      li.className = `ev ev-${r.kind}`;
      const time = document.createElement("span");
      time.className = "ev-time muted";
      time.textContent = new Date(r.t).toLocaleTimeString();
      const kind = document.createElement("span");
      kind.className = "ev-kind";
      kind.textContent = r.kind;
      const text = document.createElement("span");
      text.className = "ev-text";
      text.textContent = r.text;
      li.appendChild(time);
      li.appendChild(kind);
      li.appendChild(text);
      list.appendChild(li);
    }
  }
}
