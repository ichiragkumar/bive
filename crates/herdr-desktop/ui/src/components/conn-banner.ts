// Connection banner: non-blocking reconnecting/disconnected states. The last
// snapshot stays visible underneath; `stale` marks it as such.

import type { TauriInvoke } from "../globals.js";
import type { Snapshot, Store } from "../store.js";
import { req } from "./shared.js";

export function mountConnBanner(store: Store, invoke: TauriInvoke): void {
  const banner = req("conn-banner");

  store.subscribe((s) => {
    banner.hidden = s.connected;
    if (s.connected) return;
    banner.innerHTML = "";
    const msg = document.createElement("span");
    const retry = document.createElement("button");
    retry.className = "ghost";
    retry.textContent = "retry now";
    retry.onclick = () =>
      invoke("snapshot_cmd")
        .then((snap) => {
          store.applySnapshot(snap as Snapshot);
          store.setConnected(true);
        })
        .catch(() => {});
    const diag = document.createElement("button");
    diag.className = "ghost";
    diag.textContent = "copy diagnostics";
    diag.onclick = async () => {
      const c = store.summaryCounts();
      await navigator.clipboard.writeText(
        JSON.stringify(
          {
            time: new Date().toISOString(),
            connected: s.connected,
            stale: s.stale,
            agents: c,
          },
          null,
          2,
        ),
      );
      retry.textContent = "copied ✓";
      setTimeout(() => {
        retry.textContent = "retry now";
      }, 1200);
    };
    if (s.stale) {
      banner.classList.toggle("reconnecting", true);
      msg.textContent = "Reconnecting to daemon — showing last known fleet (stale). ";
    } else {
      banner.classList.toggle("reconnecting", false);
      msg.textContent =
        "Daemon unreachable — start it with `herdr daemon`, then retry. ";
    }
    banner.appendChild(msg);
    banner.appendChild(retry);
    banner.appendChild(diag);
  });
}
