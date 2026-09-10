// Sidebar: host groups (Local + remotes), spawn with profile picker, filter.

import type { TauriInvoke } from "../globals.js";
import type { RemoteHostEntry, Store } from "../store.js";
import { esc, req } from "./shared.js";

export const PROFILES = ["generic", "claude-code", "codex", "bash"];

export function mountSidebar(store: Store, invoke: TauriInvoke): void {
  const hostList = req("host-list");
  const profilePicker = req<HTMLSelectElement>("profile-picker");
  const filterSel = req<HTMLSelectElement>("filter");

  req("btn-spawn").onclick = async () => {
    await invoke("spawn_agent_cmd", {
      profile: profilePicker.value,
      cwd: "/tmp",
      command: window.__HERDR_SHELL__ || "/bin/bash",
      args: [],
      host: store.state.selectedHost,
    });
  };

  req("btn-add-remote").onclick = async () => {
    const name = window.prompt("Remote name (used with spawn --host):");
    if (!name) return;
    const sshTarget = window.prompt(`SSH target for "${name}" (host or user@host):`);
    if (!sshTarget) return;
    await invoke("remote_add_cmd", { name, sshTarget, port: 22, user: null });
    refreshHosts();
  };

  async function refreshHosts(): Promise<void> {
    try {
      const hosts = (await invoke("remote_list_cmd")) as RemoteHostEntry[];
      store.applyHosts(hosts);
    } catch {
      /* daemon without remotes support: sidebar stays local-only */
    }
  }
  refreshHosts();
  // Re-fetch hosts whenever the fleet changes (cheap one-shot, not polling:
  // host rows only need to exist when agents or registrations change).
  let lastFleetKey = "";
  store.subscribe((s) => {
    const key = s.cards.map((c) => c.info.id).join(",");
    if (key !== lastFleetKey) {
      lastFleetKey = key;
      refreshHosts();
    }
    renderHosts();
  });

  hostList.onclick = async (e: MouseEvent) => {
    const li = (e.target as HTMLElement | null)?.closest("li[data-host]") as HTMLElement | null;
    if (!li) return;
    if ((e.target as HTMLElement | null)?.closest("button.rm-host")) {
      await invoke("remote_remove_cmd", { name: li.dataset.host as string });
      if (store.state.selectedHost === li.dataset.host) store.state.selectedHost = null;
      refreshHosts();
      return;
    }
    store.state.selectedHost = li.dataset.host === "" ? null : (li.dataset.host as string);
    renderHosts();
  };

  filterSel.onchange = () => store.setFilter(filterSel.value);

  function renderHosts(): void {
    const s = store.state;
    // Hosts = union of registered remotes and hosts seen on live agents.
    const seen = new Map<string | null, number>();
    for (const c of s.cards) {
      const h = c.info.host || null;
      seen.set(h, (seen.get(h) || 0) + 1);
    }
    const rows: Array<[string, string, number]> = [["", "Local", seen.get(null) || 0]];
    for (const [host, up] of s.hosts) {
      rows.push([
        host.name,
        up ? esc(host.name) : `${esc(host.name)} (offline)`,
        seen.get(host.name) || 0,
      ]);
    }
    for (const [name, count] of seen) {
      if (name !== null && !rows.some((r) => r[0] === name))
        rows.push([name, esc(name), count]);
    }
    hostList.innerHTML = "";
    for (const [key, label, count] of rows) {
      const li = document.createElement("li");
      li.dataset.host = key;
      if ((store.state.selectedHost || "") === key) li.classList.add("selected");
      li.innerHTML = `<span class="host-name">${label}</span><span class="count">${count}</span>`;
      if (key !== "") {
        const rm = document.createElement("button");
        rm.className = "rm-host ghost";
        rm.title = `forget host ${key}`;
        rm.textContent = "×";
        li.appendChild(rm);
      }
      hostList.appendChild(li);
    }
  }
}
