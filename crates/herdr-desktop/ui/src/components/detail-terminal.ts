// Detail Terminal tab: raw ANSI log pane with follow/pause, full-buffer
// load, copy, and tail-size info. Ground truth when Chat mis-groups.

import type { TauriInvoke } from "../globals.js";
import type { Store } from "../store.js";
import { ansiToHtml, req } from "./shared.js";
import { toast, toastCopy } from "./toasts.js";

export function mountDetailTerminal(store: Store, invoke: TauriInvoke): void {
  const panel = req("tab-terminal");
  const logEl = req("detail-log");
  const followBtn = req("btn-follow");
  const pill = req<HTMLButtonElement>("paused-pill");
  const meta = req("terminal-meta");

  // Pause bookkeeping: frozen length + unseen lines while paused.
  let frozenId: string | null = null;
  let frozenLen = 0;
  let unseen = 0;

  followBtn.onclick = () => store.setFollow(!store.state.follow);
  pill.onclick = () => store.setFollow(true);
  req("btn-copy-logs").onclick = async (e) => {
    const card = store.state.cards.find((c) => c.info.id === store.state.selectedId);
    if (card) {
      await navigator.clipboard.writeText(card.log_tail || "");
      toastCopy(e.target as HTMLButtonElement, "copied ✓");
    }
  };
  req("btn-load-full").onclick = async () => {
    const id = store.state.selectedId;
    if (!id) return;
    try {
      const payload = (await invoke("logs_cmd", { agentId: id, bytes: 262144 })) as string;
      store.setLogTail(id, payload);
      toast("ok", "Full buffer loaded");
    } catch (e) {
      toast("err", `Load failed: ${String(e)}`);
    }
  };

  store.subscribe((s) => {
    followBtn.textContent = `follow: ${s.follow ? "on" : "off"}`;
    const card = s.cards.find((c) => c.info.id === s.selectedId);
    if (!card || store.tabFor(card.info.id, card.info.profile) !== "terminal") return;
    const tail = card.log_tail || "";
    if (s.follow || frozenId !== card.info.id) {
      logEl.innerHTML = ansiToHtml(tail);
      frozenId = card.info.id;
      frozenLen = tail.length;
      unseen = 0;
      pill.hidden = true;
      panel.scrollTop = panel.scrollHeight;
    } else {
      const fresh = tail.slice(frozenLen);
      unseen += fresh.split("\n").length - 1;
      frozenLen = tail.length;
      if (unseen > 0) {
        pill.hidden = false;
        pill.textContent = `Paused · ${unseen} new · Resume`;
      }
    }
    meta.textContent = `${tail.length.toLocaleString()} chars shown · ring buffer 256 KB`;
  });
}
