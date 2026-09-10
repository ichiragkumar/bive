// herdr desktop — entry: subscribes the store, dispatches to one module per
// surface. No polling: every update comes from a `herdr://event` push
// (or a one-shot command).
//
// Runs in exactly two places: the Tauri shell (which injects
// `window.__TAURI__`) and the generated static preview (whose harness shims
// it). Anywhere else we render an explanatory notice instead of a blank page.

import { createStore } from "./store.js";
import type { Snapshot } from "./store.js";
import { mountTopbar } from "./components/topbar.js";
import { mountTray } from "./components/tray.js";
import { mountSidebar } from "./components/sidebar.js";
import { mountAgentList } from "./components/agent-list.js";
import { mountDetail } from "./components/detail.js";
import { mountDetailChat } from "./components/detail-chat.js";
import { mountDetailTerminal } from "./components/detail-terminal.js";
import { mountDetailEvents } from "./components/detail-events.js";
import { mountDetailMedia } from "./components/detail-media.js";
import { mountDetailInfo } from "./components/detail-info.js";
import { mountComposer } from "./components/composer.js";
import { mountConnBanner } from "./components/conn-banner.js";
import { mountPalette } from "./components/palette.js";
import { confirmModal } from "./components/modal.js";
import { mountSpawnEntry, openSpawnModal } from "./components/spawn-modal.js";
import { toast } from "./components/toasts.js";
import { req } from "./components/shared.js";

function fatal(msg: string): void {
  const el = req("fatal");
  el.hidden = false;
  el.textContent = msg;
}

if (!window.__TAURI__) {
  fatal(
    "You opened the Herdr UI outside its host — this looks like a plain browser " +
      "tab, where it cannot reach the daemon. Run the desktop app instead: " +
      "1) start the daemon with `herdr daemon`, " +
      "2) `cargo run -p herdr-desktop --features tauri`. " +
      "Or open the static preview: `python3 scripts/make_preview.py`, then " +
      "open `target/preview/index.html`.",
  );
  throw new Error("missing window.__TAURI__");
}

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const store = createStore();

mountTopbar(store, invoke);
mountTray(store);
mountSidebar(store, invoke);
mountSpawnEntry(store, invoke);
mountAgentList(store, invoke, (id: string) => store.select(id));
mountDetail(store, invoke);
mountDetailChat(store);
mountDetailTerminal(store, invoke);
mountDetailEvents(store);
mountDetailMedia(store);
mountDetailInfo(store);
mountComposer(store, invoke);
mountConnBanner(store, invoke);
mountPalette(store, invoke);

// ---- event subscriptions (the only update path) ---------------------------
await listen("herdr://event", ({ payload }: { payload: any }) => {
  const ev = payload && (payload.event || payload);
  for (const notice of store.applyEvent(ev)) {
    // Optimistic-spawn reconciliation (J1).
    const m = notice.match(/^spawned:([0-9a-f]+):(\S+)$/);
    if (m) {
      toast("ok", `Spawned ${m[1].slice(0, 12)} (${m[2]})`);
      req("composer").focus();
    }
  }
});

await listen("herdr://conn", ({ payload }: { payload: any }) => {
  if (payload === "connected") {
    const wasDown = !store.state.connected || store.state.stale;
    store.setConnected(true);
    invoke("snapshot_cmd").then((snap) => {
      store.applySnapshot(snap as Snapshot);
      if (wasDown) {
        const n = store.state.cards.length;
        toast("ok", `Reconnected — resynced ${n} agent${n === 1 ? "" : "s"}`);
      }
    });
  } else {
    // "reconnecting" and "disconnected": keep the last snapshot, mark stale.
    store.setConnected(false);
  }
});

// Tray menu "Spawn shell" opens the window-side spawn flow.
await listen("herdr://open-spawn", () => openSpawnModal(store, invoke));

// First paint: one snapshot, then events keep it fresh. A refused snapshot
// means the daemon is down — the banner + empty states explain, nothing
// throws and no action silently fails (failures surface inline instead).
invoke("snapshot_cmd")
  .then((snap) => {
    store.applySnapshot(snap as Snapshot);
    store.setConnected(true);
  })
  .catch(() => {
    store.setConnected(false);
  });

// Global shortcuts: palette, spawn, kill-selected (confirmed), tabs, follow.
document.addEventListener("keydown", (e: KeyboardEvent) => {
  const inField = /INPUT|SELECT|TEXTAREA/.test(document.activeElement?.tagName ?? "");
  if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "n") {
    e.preventDefault();
    openSpawnModal(store, invoke);
    return;
  }
  if ((e.metaKey || e.ctrlKey) && e.shiftKey && e.key.toLowerCase() === "k") {
    e.preventDefault();
    const id = store.state.selectedId;
    if (id) {
      confirmModal({
        title: "Kill agent",
        body: `Kill agent ${id}? The PTY is destroyed; ring-buffer history stays until resync drops it.`,
        confirmLabel: "kill agent",
      }).then((ok) => {
        if (ok) invoke("kill_agent_cmd", { agentId: id });
      });
    }
    return;
  }
  if (inField) return;
  const id = store.state.selectedId;
  if (e.key >= "1" && e.key <= "5" && id) {
    const tabs = ["thread", "terminal", "events", "media", "info"];
    store.setTab(id, tabs[Number(e.key) - 1]);
  } else if (e.key === "f") {
    store.setFollow(!store.state.follow);
  } else if ((e.key === "g" || e.key === "G") && id) {
    store.setTab(id, "terminal");
    store.setFollow(e.key === "G");
    requestAnimationFrame(() => {
      const panel = document.getElementById("tab-terminal");
      if (panel) panel.scrollTop = e.key === "G" ? panel.scrollHeight : 0;
    });
  }
});

// View-clock tick for relative times (no daemon traffic).
setInterval(() => store.touch(), 30000);

// Introspection hook (also used by the static preview harness).
window.__HERDR_APP__ = {
  get cards() {
    return store.state.cards;
  },
  get store() {
    return store;
  },
  applySnapshot: (snap: Snapshot) => store.applySnapshot(snap),
};
