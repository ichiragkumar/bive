// Detail Info tab: explicit agent metadata (previously implicit/missing).

import { stateName } from "./shared.js";

function uptime(startedMs) {
  const s = Math.max(0, Math.floor((Date.now() - startedMs) / 1000));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`;
  return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
}

export function mountDetailInfo(store) {
  const list = document.getElementById("detail-info-list");

  store.subscribe((s) => {
    const card = s.cards.find((c) => c.info.id === s.selectedId);
    if (!card || store.tabFor(card.info.id, card.info.profile) !== "info") return;
    const info = card.info;
    const rows = [
      ["id", info.id],
      ["host", info.host || "local"],
      ["profile", info.profile],
      ["cwd", info.cwd],
      ["command", info.command],
      ["state", stateName(info.state)],
      ["uptime", uptime(info.started_at_unix_ms)],
      ["media blocks", String((card.media || []).length)],
    ];
    list.innerHTML = "";
    for (const [k, v] of rows) {
      const dt = document.createElement("dt");
      dt.textContent = k;
      const dd = document.createElement("dd");
      dd.textContent = v;
      list.appendChild(dt);
      list.appendChild(dd);
    }
  });
}
