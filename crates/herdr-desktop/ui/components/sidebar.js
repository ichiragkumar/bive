// Sidebar: host groups (Local + remotes), spawn with profile picker, filter.

import { esc } from "./shared.js";

export const PROFILES = ["generic", "claude-code", "codex", "bash"];

function hostOf(card) {
  return card.info.host || null;
}

export function mountSidebar(store, invoke) {
  const hostList = document.getElementById("host-list");
  const profilePicker = document.getElementById("profile-picker");
  const filterSel = document.getElementById("filter");

  document.getElementById("btn-spawn").onclick = async () => {
    await invoke("spawn_agent_cmd", {
      profile: profilePicker.value,
      cwd: "/tmp",
      command: window.__HERDR_SHELL__ || "/bin/bash",
      args: [],
      host: store.state.selectedHost,
    });
  };

  document.getElementById("btn-add-remote").onclick = async () => {
    const name = window.prompt("Remote name (used with spawn --host):");
    if (!name) return;
    const sshTarget = window.prompt(`SSH target for "${name}" (host or user@host):`);
    if (!sshTarget) return;
    await invoke("remote_add_cmd", { name, sshTarget, port: 22, user: null });
    refreshHosts();
  };

  async function refreshHosts() {
    try {
      const hosts = await invoke("remote_list_cmd");
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
    renderHosts(s);
  });

  hostList.onclick = async (e) => {
    const li = e.target.closest("li[data-host]");
    if (!li) return;
    if (e.target.closest("button.rm-host")) {
      await invoke("remote_remove_cmd", { name: li.dataset.host });
      if (store.state.selectedHost === li.dataset.host) store.state.selectedHost = null;
      refreshHosts();
      return;
    }
    store.state.selectedHost = li.dataset.host === "" ? null : li.dataset.host;
    renderHosts(store.state);
  };

  filterSel.onchange = () => store.setFilter(filterSel.value);

  function renderHosts(s) {
    // Hosts = union of registered remotes and hosts seen on live agents.
    const seen = new Map(); // name|null → count
    for (const c of s.cards) {
      const h = hostOf(c);
      seen.set(h, (seen.get(h) || 0) + 1);
    }
    const rows = [["", "Local", seen.get(null) || 0]];
    for (const [host, up] of s.hosts) {
      const name = Array.isArray(host) ? host[0].name : host.name;
      const isUp = Array.isArray(host) ? host[1] : up;
      rows.push([name, isUp ? esc(name) : `${esc(name)} (offline)`, seen.get(name) || 0]);
    }
    for (const [name] of seen) {
      if (name !== null && !rows.some((r) => r[0] === name)) rows.push([name, esc(name), seen.get(name)]);
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
