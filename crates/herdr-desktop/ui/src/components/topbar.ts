// Topbar: connection dot, fleet summary, kill-all. Subscribes to the store.

import type { TauriInvoke } from "../globals.js";
import type { AgentCard, Store } from "../store.js";
import { req, stateName } from "./shared.js";

export function fleetDot(cards: AgentCard[]): string {
  if (!cards.length) return "gray";
  if (cards.some((c) => stateName(c.info.state) === "Errored")) return "red";
  if (cards.some((c) => stateName(c.info.state) === "Blocked")) return "amber";
  return "green";
}

export function mountTopbar(store: Store, invoke: TauriInvoke): void {
  const connDot = req("conn-dot");
  const fleetDotEl = req("fleet-dot");
  const summary = req("fleet-summary");

  req("btn-kill-all").onclick = () => invoke("kill_all_cmd");
  req("btn-side").onclick = () => document.body.classList.toggle("no-side");

  store.subscribe((s) => {
    connDot.classList.toggle("connected", s.connected);
    const counts: Record<string, number> = { Working: 0, Blocked: 0, Errored: 0 };
    for (const c of s.cards) {
      const n = stateName(c.info.state);
      if (n in counts) counts[n] += 1;
    }
    const total = s.cards.length;
    summary.textContent = total
      ? `${total} agent${total === 1 ? "" : "s"} — ${counts["Working"]} working, ${counts["Blocked"]} blocked, ${counts["Errored"]} errored`
      : "no agents";
    fleetDotEl.className = "dot " + fleetDot(s.cards);
  });
}
