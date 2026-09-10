// Detail pane shell: persistent panel (no modal), tab switching with
// per-agent memory, kill/copy actions. Tab content owned by detail-* modules.

export function mountDetail(store, invoke) {
  const title = document.getElementById("detail-title");
  const tabs = [...document.querySelectorAll("#detail-header .tab")];
  const panels = {
    chat: document.getElementById("tab-chat"),
    terminal: document.getElementById("tab-terminal"),
    info: document.getElementById("tab-info"),
  };

  tabs.forEach((btn) => {
    btn.onclick = () => {
      const id = store.state.selectedId;
      if (id) store.setTab(id, btn.dataset.tab);
      else showTab("chat");
    };
  });

  document.getElementById("detail-kill").onclick = async () => {
    const id = store.state.selectedId;
    if (id) await invoke("kill_agent_cmd", { agentId: id });
  };
  document.getElementById("detail-copy").onclick = () => {
    const id = store.state.selectedId;
    if (id) navigator.clipboard.writeText(id);
  };

  function showTab(tab) {
    tabs.forEach((b) => b.classList.toggle("active", b.dataset.tab === tab));
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
