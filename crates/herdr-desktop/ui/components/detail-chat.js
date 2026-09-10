// Detail Chat tab: turn-by-turn view over snapshot segments (authoritative,
// computed in Rust) with a live-append mirror for lines arriving between
// snapshots. bash/generic profiles show a "raw output only" notice pointing
// at the Terminal tab instead of an empty Chat.

import { esc } from "./shared.js";

const RAW_PROFILES = new Set(["bash", "generic"]);

export function mountDetailChat(store) {
  const panel = document.getElementById("tab-chat");
  const turnsEl = document.getElementById("chat-turns");
  const mediaEl = document.getElementById("chat-media");

  store.subscribe((s) => {
    const card = s.cards.find((c) => c.info.id === s.selectedId);
    if (!card || store.tabFor(card.info.id, card.info.profile) !== "chat") return;
    renderChat(card, s.chat[card.info.id] || []);
  });

  function renderChat(card, turns) {
    mediaEl.innerHTML = "";
    for (const m of card.media || []) {
      const fig = document.createElement("figure");
      const img = document.createElement("img");
      img.src = `data:${m.mime};base64,${m.data_base64}`;
      if (m.caption) {
        const cap = document.createElement("figcaption");
        cap.textContent = m.caption;
        fig.appendChild(cap);
      }
      fig.prepend(img);
      mediaEl.appendChild(fig);
    }
    turnsEl.innerHTML = "";
    if (RAW_PROFILES.has(card.info.profile)) {
      turnsEl.innerHTML =
        `<div class="chat-notice">Raw output only for <b>${esc(card.info.profile)}</b> — ` +
        `see the Terminal tab. Chat turns are parsed for claude-code / codex agents.</div>`;
      return;
    }
    if (!turns.length) {
      turnsEl.innerHTML = `<div class="chat-notice">No turns yet — output will appear here.</div>`;
      return;
    }
    for (const t of turns) {
      const div = document.createElement("div");
      div.className = `turn ${t.kind.toLowerCase()}`;
      const who = t.kind === "Human" ? "you" : t.kind === "Tool" ? "tool" : "agent";
      div.innerHTML = `<span class="who">${esc(who)}</span><pre>${esc(t.text)}</pre>`;
      turnsEl.appendChild(div);
    }
    panel.scrollTop = panel.scrollHeight;
  }
}
