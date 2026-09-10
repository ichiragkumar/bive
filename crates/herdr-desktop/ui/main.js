// herdr desktop — entry: subscribes the store, dispatches to one module per
// surface. No polling: every update comes from a `herdr://event` push
// (or a one-shot command).
//
// Runs in exactly two places: the Tauri shell (which injects
// `window.__TAURI__`) and the generated static preview (whose harness shims
// it). Anywhere else we render an explanatory notice instead of a blank page.

import { createStore } from "./store.js";
import { mountTopbar } from "./components/topbar.js";
import { mountTray } from "./components/tray.js";
import { mountSidebar } from "./components/sidebar.js";
import { mountAgentList } from "./components/agent-list.js";
import { mountDetail } from "./components/detail.js";
import { mountDetailChat } from "./components/detail-chat.js";
import { mountDetailTerminal } from "./components/detail-terminal.js";
import { mountDetailInfo } from "./components/detail-info.js";
import { mountComposer } from "./components/composer.js";

function fatal(msg) {
  const el = document.getElementById("fatal");
  el.hidden = false;
  el.textContent = msg;
}

if (!window.__TAURI__) {
  fatal(
    "herdr UI needs its host: open it in the Tauri app " +
      "(cargo run -p herdr-desktop --features tauri) or view the static preview " +
      "(python3 scripts/make_preview.py, then target/preview/index.html).",
  );
  throw new Error("missing window.__TAURI__");
}

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const store = createStore();

mountTopbar(store, invoke);
mountTray(store);
mountSidebar(store, invoke);
mountAgentList(store, (id) => store.select(id));
mountDetail(store, invoke);
mountDetailChat(store);
mountDetailTerminal(store);
mountDetailInfo(store);
mountComposer(store, invoke);

// ---- event subscriptions (the only update path) ---------------------------
await listen("herdr://event", ({ payload }) => {
  const ev = payload && (payload.event || payload);
  store.applyEvent(ev);
});

await listen("herdr://conn", ({ payload }) => {
  const connected = payload === "connected";
  store.setConnected(connected);
  if (connected) invoke("snapshot_cmd").then((snap) => store.applySnapshot(snap));
});

// First paint: one snapshot, then events keep it fresh.
invoke("snapshot_cmd")
  .then((snap) => store.applySnapshot(snap))
  .catch(() => {});

// Introspection hook (also used by the static preview harness).
window.__HERDR_APP__ = {
  get cards() {
    return store.state.cards;
  },
  get store() {
    return store;
  },
  applySnapshot: (snap) => store.applySnapshot(snap),
};
