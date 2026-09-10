// Detail pane shell: persistent panel (no modal), tab switching with
// per-agent memory, kill/copy actions. Tab content owned by detail-* modules.

import type { TauriInvoke } from "../globals.js";
import type { Store } from "../store.js";
import { req } from "./shared.js";

export function mountDetail(store: Store, invoke: TauriInvoke): void {
  const title = req("detail-title");
  const tabs = Array.from(document.querySelectorAll<HTMLElement>("#detail-header .tab"));
  const panels: Record<string, HTMLElement> = {
    chat: req("tab-chat"),
    terminal: req("tab-terminal"),
    info: req("tab-info"),
  };

  tabs.forEach((btn) => {
    btn.onclick = () => {
      const id = store.state.selectedId;
      if (id) store.setTab(id, btn.dataset["tab"] as string);
      else showTab("chat");
    };
  });

  req("detail-kill").onclick = async () => {
    const id = store.state.selectedId;
    if (id) await invoke("kill_agent_cmd", { agentId: id });
  };
  req("detail-copy").onclick = () => {
    const id = store.state.selectedId;
    if (id) navigator.clipboard.writeText(id);
  };

  function showTab(tab: string): void {
    tabs.forEach((b) => b.classList.toggle("active", b.dataset["tab"] === tab));
    for (const [name, el] of Object.entries(panels)) {
      el.hidden = name !== tab;
    }
  }

  store.subscribe((s) => {
    const card = s.cards.find((c) => c.info.id === s.selectedId);
    if (!card) {
      title.textContent = "no agent selected";
      return;
    }
    title.textContent = `${card.info.id.slice(0, 12)} · ${card.info.profile} · ${card.info.command}`;
    showTab(store.tabFor(card.info.id, card.info.profile));
  });
}
