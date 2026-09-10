// Topbar: connection pill, clickable summary chips, New agent, palette,
// ⋯ menu (destructive actions live here with confirmation — never orphaned).

import type { TauriInvoke } from "../globals.js";
import type { AgentCard, Store } from "../store.js";
import { confirmModal } from "./modal.js";
import { req, stateName } from "./shared.js";
import { toast } from "./toasts.js";

export function fleetDot(cards: AgentCard[]): string {
  if (!cards.length) return "gray";
  if (cards.some((c) => stateName(c.info.state) === "Errored")) return "red";
  if (cards.some((c) => stateName(c.info.state) === "Blocked")) return "amber";
  return "green";
}

async function copyDiagnostics(store: Store, invoke: TauriInvoke): Promise<void> {
  const c = store.summaryCounts();
  let ping = false;
  try {
    ping = (await invoke("ping_cmd")) as boolean;
  } catch {
    ping = false;
  }
  const diag = {
    app: "herdr-desktop",
    time: new Date().toISOString(),
    daemon: ping ? "reachable" : "unreachable",
    connected: store.state.connected,
    stale: store.state.stale,
    agents: { total: c.total, working: c.working, blocked: c.blocked, errored: c.errored, idle: c.idle, exited: c.exited },
    hosts: store.state.hosts.map(([h, up]) => ({ name: h.name, up })),
  };
  await navigator.clipboard.writeText(JSON.stringify(diag, null, 2));
  toast("ok", "Diagnostics copied");
}

export function mountTopbar(store: Store, invoke: TauriInvoke): void {
  const connDot = req("conn-dot");
  const connLabel = req("conn-label");
  const fleetDotEl = req("fleet-dot");
  const summary = req("fleet-summary");

  req("btn-new-agent").onclick = () =>
    document.getElementById("btn-spawn")?.click();
  req("btn-side").onclick = () => document.body.classList.toggle("no-side");
  mountMenu(store, invoke);

  store.subscribe((s) => {
    connDot.classList.toggle("connected", s.connected);
    const pill = req("conn-pill");
    if (!s.connected) {
      connLabel.textContent = "Disconnected";
      pill.className = "pill down";
      connDot.title = "daemon: unreachable";
    } else if (s.stale) {
      connLabel.textContent = "Reconnecting…";
      pill.className = "pill recon";
      connDot.title = "daemon: reconnecting";
    } else {
      connLabel.textContent = "Connected";
      pill.className = "pill up";
      connDot.title = "daemon: connected";
    }
    const c = store.summaryCounts();
    summary.innerHTML = "";
    const chips: Array<[string, string, string]> = [
      [`${c.total}`, "total", "all"],
      [`● ${c.working}`, "Working", "Working"],
      [`■ ${c.blocked}`, "Blocked", "Blocked"],
      [`✖ ${c.errored}`, "Errored", "Errored"],
      [`◌ ${c.idle}`, "Idle", "Idle"],
      [`· ${c.exited}`, "Exited", "Exited"],
    ];
    for (const [text, cls, filter] of chips) {
      const b = document.createElement("button");
      b.className = `chip ${cls}${store.state.filter === filter ? " on" : ""}`;
      b.textContent = text;
      b.title = `filter: ${filter}`;
      b.onclick = () => store.setFilter(store.state.filter === filter ? "all" : filter);
      summary.appendChild(b);
    }
    if (s.stale) {
      const stale = document.createElement("span");
      stale.className = "chip stale";
      stale.textContent = "stale";
      summary.appendChild(stale);
    }
    fleetDotEl.className = "dot " + (s.connected ? fleetDot(s.cards) : "gray");
  });
}

function mountMenu(store: Store, invoke: TauriInvoke): void {
  const btn = req<HTMLButtonElement>("btn-menu");
  let open: HTMLElement | null = null;
  const close = () => {
    open?.remove();
    open = null;
    document.removeEventListener("mousedown", outside, true);
  };
  const outside = (e: MouseEvent) => {
    if (open && !(e.target as HTMLElement).closest("#menu-pop")) close();
  };
  btn.onclick = () => {
    if (open) {
      close();
      return;
    }
    const pop = document.createElement("div");
    pop.id = "menu-pop";
    pop.setAttribute("role", "menu");
    const item = (label: string, danger: boolean, run: () => void) => {
      const b = document.createElement("button");
      b.textContent = label;
      b.setAttribute("role", "menuitem");
      if (danger) b.classList.add("danger");
      b.onclick = () => {
        close();
        run();
      };
      pop.appendChild(b);
    };
    item(`Kill all (${store.state.cards.length})…`, true, async () => {
      const n = store.state.cards.length;
      if (!n) return;
      if (
        await confirmModal({
          title: "Kill all agents",
          body: `Kill all ${n} agent${n === 1 ? "" : "s"}? This cannot be undone.`,
          confirmLabel: `kill ${n} agent${n === 1 ? "" : "s"}`,
        })
      ) {
        await invoke("kill_all_cmd");
        toast("ok", `Killed ${n} agent${n === 1 ? "" : "s"}`);
      }
    });
    item("Clear exited", false, () => {
      const n = store.hideExited();
      toast("ok", n ? `Cleared ${n} exited` : "Nothing exited to clear");
    });
    item("Shutdown daemon…", true, async () => {
      if (
        await confirmModal({
          title: "Shutdown daemon",
          body: "This stops the daemon and ALL agents (local and remote bridges). Clients will show disconnected. This cannot be undone.",
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
    });
    item("Copy diagnostics", false, () => void copyDiagnostics(store, invoke));
    document.body.appendChild(pop);
    const r = btn.getBoundingClientRect();
    pop.style.top = `${r.bottom + 6}px`;
    pop.style.right = `${Math.max(8, window.innerWidth - r.right)}px`;
    open = pop;
    document.addEventListener("mousedown", outside, true);
  };
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && open) close();
  });
}
