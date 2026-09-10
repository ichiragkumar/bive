// Topbar: connection dot, fleet summary, kill-all. Subscribes to the store.

import { stateName } from "./shared.js";

export function fleetDot(cards) {
  if (!cards.length) return "gray";
  if (cards.some((c) => stateName(c.info.state) === "Errored")) return "red";
  if (cards.some((c) => stateName(c.info.state) === "Blocked")) return "amber";
  return "green";
}

export function mountTopbar(store, invoke) {
  const connDot = document.getElementById("conn-dot");
  const fleetDotEl = document.getElementById("fleet-dot");
  const summary = document.getElementById("fleet-summary");

  document.getElementById("btn-kill-all").onclick = () => invoke("kill_all_cmd");
  document.getElementById("btn-side").onclick = () =>
    document.body.classList.toggle("no-side");

  store.subscribe((s) => {
    connDot.classList.toggle("connected", s.connected);
    const counts = { Working: 0, Blocked: 0, Errored: 0 };
    for (const c of s.cards) {
      const n = stateName(c.info.state);
      if (n in counts) counts[n] += 1;
    }
    const total = s.cards.length;
    summary.textContent = total
      ? `${total} agent${total === 1 ? "" : "s"} — ${counts.Working} working, ${counts.Blocked} blocked, ${counts.Errored} errored`
      : "no agents";
    fleetDotEl.className = "dot " + fleetDot(s.cards);
  });
}
