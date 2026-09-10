// Detail Media tab: gallery of AgentMedia blocks with mime, caption,
// and copy-data-URL controls.

import type { Store } from "../store.js";
import { req } from "./shared.js";

export function mountDetailMedia(store: Store): void {
  const gallery = req("media-gallery");

  store.subscribe((s) => {
    const card = s.cards.find((c) => c.info.id === s.selectedId);
    if (!card || store.tabFor(card.info.id, card.info.profile) !== "media") return;
    gallery.innerHTML = "";
    const media = card.media || [];
    if (!media.length) {
      gallery.innerHTML = `<div class="muted">No media from this agent yet.</div>`;
      return;
    }
    media.forEach((m, i) => {
      const fig = document.createElement("figure");
      const url = `data:${m.mime};base64,${m.data_base64}`;
      if (m.mime.startsWith("image/")) {
        const img = document.createElement("img");
        img.src = url;
        img.alt = m.caption || `media ${i + 1}`;
        fig.appendChild(img);
      } else {
        const div = document.createElement("div");
        div.className = "media-blob muted";
        div.textContent = m.mime;
        fig.appendChild(div);
      }
      const cap = document.createElement("figcaption");
      cap.textContent = `${m.caption || `media ${i + 1}`} · ${m.mime}`;
      fig.appendChild(cap);
      const copy = document.createElement("button");
      copy.className = "ghost";
      copy.textContent = "copy data URL";
      copy.onclick = () => navigator.clipboard.writeText(url);
      fig.appendChild(copy);
      gallery.appendChild(fig);
    });
  });
}
