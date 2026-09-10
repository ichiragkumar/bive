// Sidebar: navigation, not a form. FLEET state filters + HOSTS with counts +
// profile quick-spawn chips. Single-select within a section; counts live.

import type { TauriInvoke } from "../globals.js";
import type { Store } from "../store.js";
import { confirmModal } from "./modal.js";
import { esc, req } from "./shared.js";
import { openSpawnModal } from "./spawn-modal.js";
import { toast } from "./toasts.js";

const FLEET_ROWS: Array<[string, string, string]> = [
  ["all", "All", "●"],
  ["attention", "Needs attention", "■"],
  ["Working", "Working", "●"],
  ["Blocked", "Blocked", "■"],
  ["Errored", "Errored", "✖"],
  ["Idle", "Idle", "◌"],
  ["Exited", "Exited", "·"],
];

const PROFILE_CHIPS: Array<[string, string]> = [
  ["bash", "Bash"],
  ["claude-code", "Claude Code"],
  ["codex", "Codex"],
  ["generic", "Generic"],
];

export function mountSidebar(store: Store, invoke: TauriInvoke): void {
  const hostList = req("host-list");
  const fleetList = req("fleet-filters");
  const chips = req("profile-chips");

  for (const [profile, label] of PROFILE_CHIPS) {
    const b = document.createElement("button");
    b.className = "ghost chip-btn";
    b.textContent = label;
    b.title = `spawn ${label} agent…`;
    b.onclick = () => openSpawnModal(store, invoke, { profile });
    chips.appendChild(b);
  }

  req("btn-add-remote").onclick = async () => {
    const name = window.prompt("Remote name (used with spawn --host):");
    if (!name) return;
    const sshTarget = window.prompt(`SSH target for "${name}" (host or user@host):`);
    if (!sshTarget) return;
    try {
      await invoke("remote_add_cmd", { name, sshTarget, port: 22, user: null });
      toast("ok", `Remote ${name} added`);
    } catch (e) {
      toast("err", `Add remote failed: ${String(e)}`);
    }
    refreshHosts();
  };

  async function refreshHosts(): Promise<void> {
    try {
      const hosts = (await invoke("remote_list_cmd")) as import("../store.js").RemoteHostEntry[];
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
    renderFleet(s.filter);
    renderHosts();
  });

  hostList.onclick = async (e: MouseEvent) => {
    const li = (e.target as HTMLElement | null)?.closest("li[data-host]") as HTMLElement | null;
    if (!li) return;
    if ((e.target as HTMLElement | null)?.closest("button.rm-host")) {
      const name = li.dataset["host"] as string;
      const live = store.state.cards.filter((c) => (c.info.host || null) === name).length;
      if (
        await confirmModal({
          title: "Forget host",
          body: live
            ? `Forget host ${name}? Its ${live} live agent${live === 1 ? "" : "s"} will be disconnected.`
            : `Forget host ${name}?`,
          confirmLabel: "forget host",
        })
      ) {
        try {
          await invoke("remote_remove_cmd", { name });
          toast("ok", `Host ${name} forgotten`);
        } catch (err) {
          toast("err", `Remove failed: ${String(err)}`);
        }
        if (store.state.selectedHost === name) store.state.selectedHost = null;
        refreshHosts();
      }
      return;
    }
    store.state.selectedHost = li.dataset["host"] === "" ? null : (li.dataset["host"] as string);
    renderHosts();
  };

  fleetList.onclick = (e: MouseEvent) => {
    const li = (e.target as HTMLElement | null)?.closest("li[data-filter]") as HTMLElement | null;
    if (!li) return;
    store.setFilter(li.dataset["filter"] as string);
  };

  function renderFleet(active: string): void {
    const c = store.summaryCounts();
    const counts: Record<string, number> = {
      all: c.total,
      attention: c.blocked + c.errored,
      Working: c.working,
      Blocked: c.blocked,
      Errored: c.errored,
      Idle: c.idle,
      Exited: c.exited,
    };
    fleetList.innerHTML = "";
    for (const [key, label, glyph] of FLEET_ROWS) {
      const li = document.createElement("li");
      if (active === key) li.classList.add("selected");
      li.dataset["filter"] = key;
      li.innerHTML =
        `<span class="glyph">${glyph}</span><span class="host-name">${label}</span>` +
        `<span class="count">${counts[key] ?? 0}</span>`;
      fleetList.appendChild(li);
    }
  }

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
      li.dataset["host"] = key;
      if ((store.state.selectedHost || "") === key) li.classList.add("selected");
      li.innerHTML = `<span class="host-name">${label}</span><span class="count">${count}</span>`;
      if (key !== "") {
        const rm = document.createElement("button");
        rm.className = "rm-host ghost";
        rm.title = `forget host ${key}`;
        rm.setAttribute("aria-label", `forget host ${key}`);
        rm.textContent = "×";
        li.appendChild(rm);
      }
      hostList.appendChild(li);
    }
  }
}
