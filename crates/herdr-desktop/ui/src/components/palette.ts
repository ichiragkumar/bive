// Command palette: fuzzy action list (spawn, jump-to-agent, kill, copy).
// Opens with Cmd/Ctrl+K. Arrows navigate, Enter runs, Esc closes.

import type { TauriInvoke } from "../globals.js";
import type { Store } from "../store.js";
import { confirmModal } from "./modal.js";
import { req, stateName } from "./shared.js";
import { openSpawnModal } from "./spawn-modal.js";
import { toast } from "./toasts.js";

interface Action {
  id: string;
  label: string;
  hint: string;
  run: () => void | Promise<void>;
}

export function mountPalette(store: Store, invoke: TauriInvoke): void {
  const root = req("palette-root");
  req("btn-palette").onclick = open;

  function open(): void {
    root.innerHTML = "";
    root.hidden = false;
    const box = document.createElement("div");
    box.className = "palette";
    box.setAttribute("role", "dialog");
    box.setAttribute("aria-label", "command palette");
    const input = document.createElement("input");
    input.placeholder = "type a command or agent id…";
    input.setAttribute("aria-label", "command palette");
    const list = document.createElement("ul");
    box.appendChild(input);
    box.appendChild(list);
    root.appendChild(box);

    const prevFocus = document.activeElement as HTMLElement | null;
    const close = () => {
      root.hidden = true;
      root.innerHTML = "";
      document.removeEventListener("keydown", onKey, true);
      prevFocus?.focus?.();
    };

    const actions = buildActions();
    let filtered = actions;
    let idx = 0;

    const render = () => {
      list.innerHTML = "";
      filtered.forEach((a, i) => {
        const li = document.createElement("li");
        li.className = i === idx ? "active" : "";
        li.innerHTML = "";
        const label = document.createElement("span");
        label.textContent = a.label;
        const hint = document.createElement("span");
        hint.className = "muted";
        hint.textContent = a.hint;
        li.appendChild(label);
        li.appendChild(hint);
        li.onclick = () => {
          close();
          void a.run();
        };
        list.appendChild(li);
      });
    };

    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        close();
      } else if (e.key === "ArrowDown") {
        e.preventDefault();
        idx = Math.min(filtered.length - 1, idx + 1);
        render();
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        idx = Math.max(0, idx - 1);
        render();
      } else if (e.key === "Enter") {
        const a = filtered[idx];
        if (a) {
          close();
          void a.run();
        }
      }
    };
    document.addEventListener("keydown", onKey, true);
    input.addEventListener("input", () => {
      const q = input.value.trim().toLowerCase();
      filtered = q
        ? actions.filter((a) => `${a.label} ${a.hint}`.toLowerCase().includes(q))
        : actions;
      idx = 0;
      render();
    });
    root.onmousedown = (e) => {
      if (e.target === root) close();
    };
    render();
    input.focus();
  }

  function buildActions(): Action[] {
    const s = store.state;
    const sel = s.cards.find((c) => c.info.id === s.selectedId);
    const out: Action[] = [
      { id: "spawn", label: "Spawn agent…", hint: "profile · host · cwd", run: () => openSpawnModal(store, invoke) },
    ];
    for (const [profile, label] of [
      ["bash", "Spawn Bash shell"],
      ["claude-code", "Spawn Claude Code"],
      ["codex", "Spawn Codex"],
      ["generic", "Spawn generic command"],
    ] as Array<[string, string]>) {
      out.push({
        id: `preset-${profile}`,
        label,
        hint: "spawn preset",
        run: () => openSpawnModal(store, invoke, { profile }),
      });
    }
    for (const c of s.cards) {
      const id = c.info.id;
      out.push({
        id: `go-${id}`,
        label: `Go to ${id.slice(0, 12)}`,
        hint: `${c.info.profile} · ${stateName(c.info.state)}`,
        run: () => store.select(id),
      });
    }
    for (const [host] of s.hosts) {
      out.push({
        id: `host-${host.name}`,
        label: `Switch host → ${host.name}`,
        hint: "sidebar filter",
        run: () => {
          store.state.selectedHost = host.name;
        },
      });
    }
    out.push({
      id: "host-local",
      label: "Switch host → local",
      hint: "sidebar filter",
      run: () => {
        store.state.selectedHost = null;
      },
    });
    if (sel) {
      const id = sel.info.id;
      out.push(
        {
          id: "copy",
          label: `Copy id ${id.slice(0, 12)}`,
          hint: "clipboard",
          run: () => navigator.clipboard.writeText(id),
        },
        {
          id: "follow",
          label: `${store.state.follow ? "Pause" : "Resume"} follow`,
          hint: "terminal tail",
          run: () => store.setFollow(!store.state.follow),
        },
        {
          id: "kill",
          label: `Kill ${id.slice(0, 12)}…`,
          hint: "confirm",
          run: async () => {
            if (await confirmModal({
              title: "Kill agent",
              body: `Kill agent ${id}? The PTY is destroyed; ring-buffer history stays until resync drops it.`,
              confirmLabel: "kill agent",
            })) {
              await invoke("kill_agent_cmd", { agentId: id });
            }
          },
        },
      );
      for (const tab of ["thread", "terminal", "events", "info"]) {
        out.push({
          id: `tab-${tab}`,
          label: `Switch tab → ${tab}`,
          hint: "detail pane",
          run: () => store.setTab(id, tab),
        });
      }
    } else {
      out.push({
        id: "kill-none",
        label: "Kill selected",
        hint: "no agent selected",
        run: () => toast("info", "No agent selected"),
      });
    }
    if (s.cards.length) {
      out.push({
        id: "kill-all",
        label: `Kill all ${s.cards.length} agents…`,
        hint: "confirm",
        run: async () => {
          if (await confirmModal({
            title: "Kill all agents",
            body: `Kill all ${s.cards.length} agents? This cannot be undone.`,
            confirmLabel: `kill ${s.cards.length} agents`,
          })) {
            await invoke("kill_all_cmd");
          }
        },
      });
    }
    out.push(
      {
        id: "reconnect",
        label: "Reconnect now",
        hint: "resync snapshot",
        run: async () => {
          try {
            const snap = (await invoke("snapshot_cmd")) as import("../store.js").Snapshot;
            store.applySnapshot(snap);
            store.setConnected(true);
            toast("ok", "Reconnected");
          } catch {
            toast("err", "Still unreachable — is the daemon running?");
          }
        },
      },
      {
        id: "shutdown",
        label: "Shutdown daemon…",
        hint: "confirm",
        run: async () => {
          if (
            await confirmModal({
              title: "Shutdown daemon",
              body: "This stops the daemon and ALL agents. This cannot be undone.",
              confirmLabel: "shutdown daemon",
            })
          ) {
            try {
              await invoke("shutdown_cmd");
              toast("ok", "Daemon shutting down");
            } catch (e) {
              toast("err", `Shutdown failed: ${String(e)}`);
            }
          }
        },
      },
      {
        id: "diag",
        label: "Copy diagnostics",
        hint: "clipboard",
        run: async () => {
          const c = store.summaryCounts();
          await navigator.clipboard.writeText(
            JSON.stringify({ time: new Date().toISOString(), agents: c }, null, 2),
          );
          toast("ok", "Diagnostics copied");
        },
      },
    );
    return out;
  }

  // Global opener.
  document.addEventListener("keydown", (e: KeyboardEvent) => {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
      e.preventDefault();
      if (root.hidden) open();
    }
  });
}
