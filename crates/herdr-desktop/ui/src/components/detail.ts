// Detail pane shell: persistent panel (no modal), tab switching with
// per-agent memory, kill/copy actions. Tab content owned by detail-* modules.

import type { TauriInvoke } from "../globals.js";
import type { Store } from "../store.js";
import { confirmModal } from "./modal.js";
import { req, stateName } from "./shared.js";
import { openSpawnModal } from "./spawn-modal.js";
import { toast, toastCopy } from "./toasts.js";

export function mountDetail(store: Store, invoke: TauriInvoke): void {
  const title = req("detail-title");
  const tabs = Array.from(document.querySelectorAll<HTMLElement>("#detail-header .tab"));
  const mediaTabBtn = req("tabbtn-media");
  const emptyPane = req("detail-empty");
  const tabsRow = document.querySelector<HTMLElement>("#detail-header .tabs");
  req("btn-empty-spawn").onclick = () =>
    document.getElementById("btn-spawn")?.click();
  const headerActions = [req("detail-copy"), req("detail-kill")];
  const exitedBar = req("exited-bar");
  const exitedText = req("exited-text");
  req("btn-copy-exit-logs").onclick = async (e) => {
    const card = store.state.cards.find((c) => c.info.id === store.state.selectedId);
    if (card) {
      await navigator.clipboard.writeText(card.log_tail || "");
      toastCopy(e.target as HTMLButtonElement);
    }
  };
  req("btn-respawn-like").onclick = () => {
    const card = store.state.cards.find((c) => c.info.id === store.state.selectedId);
    if (card) {
      // Prefill what the protocol stores (profile/host/cwd + full command
      // line); args can't be recovered exactly, so the user reviews them.
      openSpawnModal(store, invoke, {
        profile: card.info.profile,
        host: card.info.host,
        cwd: card.info.cwd,
        command: card.info.command,
        args: "",
      });
    }
  };
  req("btn-kill-exited").onclick = async () => {
    const id = store.state.selectedId;
    if (!id) return;
    if (
      await confirmModal({
        title: "Kill agent",
        body: `Kill agent ${id}? The PTY is destroyed; ring-buffer history stays until resync drops it.`,
        confirmLabel: "kill agent",
      })
    ) {
      await invoke("kill_agent_cmd", { agentId: id });
    }
  };
  const panels: Record<string, HTMLElement> = {
    thread: req("tab-chat"),
    terminal: req("tab-terminal"),
    events: req("tab-events"),
    media: req("tab-media"),
    info: req("tab-info"),
  };

  tabs.forEach((btn) => {
    btn.onclick = () => {
      const id = store.state.selectedId;
      if (id) store.setTab(id, btn.dataset["tab"] as string);
      else showTab("thread");
    };
  });

  req("detail-kill").onclick = async () => {
    const id = store.state.selectedId;
    if (!id) return;
    if (
      await confirmModal({
        title: "Kill agent",
        body: `Kill agent ${id}? The PTY is destroyed; ring-buffer history stays until resync drops it.`,
        confirmLabel: "kill agent",
      })
    ) {
      await invoke("kill_agent_cmd", { agentId: id });
    }
  };
  req("detail-copy").onclick = () => {
    const id = store.state.selectedId;
    if (id) navigator.clipboard.writeText(id);
  };

  function showTab(tab: string): void {
    emptyPane.hidden = true;
    tabs.forEach((b) => {
      const active = b.dataset["tab"] === tab;
      b.classList.toggle("active", active);
      b.setAttribute("aria-selected", String(active));
    });
    for (const [name, el] of Object.entries(panels)) {
      el.hidden = name !== tab;
    }
  }

  function showEmpty(): void {
    for (const el of Object.values(panels)) el.hidden = true;
    tabs.forEach((b) => {
      b.classList.toggle("active", false);
      b.setAttribute("aria-selected", "false");
    });
    if (tabsRow) tabsRow.hidden = true;
    // No target → no enabled-looking inert controls (guarded by test).
    for (const b of headerActions) b.hidden = true;
    exitedBar.hidden = true;
    emptyPane.hidden = false;
  }

  store.subscribe((s) => {
    const card = s.cards.find((c) => c.info.id === s.selectedId);
    if (!card) {
      title.textContent = "no agent selected";
      showEmpty();
      return;
    }
    for (const b of headerActions) b.hidden = false;
    if (tabsRow) tabsRow.hidden = false;
    emptyPane.hidden = true;
    const st = stateName(card.info.state);
    const dead = st === "Exited" || st === "Errored";
    exitedBar.hidden = !dead;
    if (dead) exitedText.textContent = `Agent ${st === "Exited" ? "exited" : "errored"} — sending is disabled.`;
    mediaTabBtn.hidden = !(card.media && card.media.length);
    title.textContent = `${card.info.id.slice(0, 12)} · ${card.info.profile} · ${card.info.command}`;
    showTab(store.tabFor(card.info.id, card.info.profile));
  });
}
