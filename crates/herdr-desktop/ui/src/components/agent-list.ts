// Agent list: selection-driven rows over the store's visibleAgents()
// (filter + host scope + search + attention-first sort). Sparklines render
// per-row output activity; rows expose kill/copy without opening detail.

import type { TauriInvoke } from "../globals.js";
import type { AgentCard, Snapshot, Store } from "../store.js";
import { confirmModal } from "./modal.js";
import { esc, lastLine, req, stateName } from "./shared.js";
import { openSpawnModal } from "./spawn-modal.js";

function runtimeOf(card: AgentCard): string {
  return relTime(card.info.started_at_unix_ms);
}

/** Compact relative time ("12s", "3m", "2h"). Ticks via the 30 s clock. */
export function relTime(ms: number): string {
  const s = Math.max(0, Math.floor((Date.now() - ms) / 1000));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86400)}d`;
}

function sparkBars(bars: number[]): string {
  if (!bars.length) return "";
  const max = Math.max(...bars, 1);
  return bars
    .map((v) => `<span style="height:${Math.max(2, Math.round((v / max) * 14))}px"></span>`)
    .join("");
}

export function mountAgentList(
  store: Store,
  invoke: TauriInvoke,
  onSelect: (id: string) => void,
): void {
  const list = req("agent-list");
  const search = req<HTMLInputElement>("fleet-search");
  const sort = req<HTMLSelectElement>("fleet-sort");

  search.addEventListener("input", () => store.setSearch(search.value));
  sort.onchange = () =>
    store.setSort(sort.value as "attention" | "newest" | "activity" | "state");

  store.subscribe((s) => renderList(store.visibleAgents(), s.selectedId));

  /** Bounded render (J2/H): first 200 rows + notice instead of virtual DOM. */
  function renderList(visible: AgentCard[], selectedId: string | null): void {
    list.innerHTML = "";
    renderPending();
    if (!visible.length && !store.state.pendingSpawns.length) {
      renderEmpty();
      return;
    }
    const CAP = 200;
    for (const card of visible.slice(0, CAP)) {
      list.appendChild(renderRow(card, selectedId));
    }
    if (visible.length > CAP) {
      const li = document.createElement("li");
      li.className = "muted cap-notice";
      li.textContent = `Showing ${CAP} of ${visible.length} — refine search or filters.`;
      list.appendChild(li);
    }
  }

  function renderPending(): void {
    for (const p of store.state.pendingSpawns) {
      const li = document.createElement("li");
      li.className = "agent-row pending";
      li.innerHTML =
        `<span class="id muted">spawning…</span>` +
        `<span class="profile">${esc(p.profile)}</span>` +
        `<span class="badge Starting">…</span>` +
        `<span class="last-line muted">waiting for AgentSpawned</span>`;
      list.appendChild(li);
    }
  }

  function renderRow(card: AgentCard, selectedId: string | null): HTMLElement {
    const li = document.createElement("li");
    li.className = "agent-row";
    if (card.info.id === selectedId) li.classList.add("selected");
    const st = stateName(card.info.state);
    if (st === "Blocked" || st === "Errored") li.classList.add("needs-attention");
    const bars = sparkBars(store.state.activity[card.info.id] || []);
    const host = card.info.host || "local";
    li.innerHTML =
      `<span class="id">${esc(card.info.id.slice(0, 12))}</span>` +
      `<span class="profile">${esc(card.info.profile)}</span>` +
      `<span class="badge ${esc(st)}">${esc(st)}</span>` +
      `<span class="last-line">${esc(lastLine(card.log_tail || "")) || "&nbsp;"}</span>` +
      `<span class="meta muted">${esc(host)} · ${esc(card.info.cwd)} · ${runtimeOf(card)} · active ${relTime(card.info.last_output_unix_ms)} ago</span>` +
      `<span class="spark" aria-hidden="true">${bars}</span>`;
    li.onclick = () => onSelect(card.info.id);
    const copy = document.createElement("button");
    copy.className = "row-copy ghost";
    copy.title = `copy id ${card.info.id}`;
    copy.setAttribute("aria-label", `copy id ${card.info.id}`);
    copy.textContent = "⧉";
    copy.onclick = async (e) => {
      e.stopPropagation();
      await navigator.clipboard.writeText(card.info.id);
      copy.textContent = "✓";
      setTimeout(() => {
        copy.textContent = "⧉";
      }, 1200);
    };
    const kill = document.createElement("button");
    kill.className = "row-kill ghost";
    kill.title = `kill ${card.info.id}`;
    kill.setAttribute("aria-label", `kill ${card.info.id}`);
    kill.textContent = "×";
    kill.onclick = async (e) => {
      e.stopPropagation();
      if (
        await confirmModal({
          title: "Kill agent",
          body: `Kill agent ${card.info.id} (${card.info.profile}, last line: ${lastLine(card.log_tail || "").slice(0, 80)})? This can't be undone.`,
          confirmLabel: "kill agent",
        })
      ) {
        await invoke("kill_agent_cmd", { agentId: card.info.id });
      }
    };
    li.appendChild(copy);
    li.appendChild(kill);
    return li;
  }

  function renderEmpty(): void {
    const s = store.state;
    const li = document.createElement("li");
    li.className = "empty-state";
    if (!s.connected) {
      li.innerHTML =
        `<h3>Daemon unavailable</h3>` +
        `<p>The desktop cannot reach the daemon socket. Start it, then retry — this view resyncs automatically.</p>`;
      const retry = document.createElement("button");
      retry.textContent = "retry now";
      retry.onclick = () =>
        invoke("snapshot_cmd").then((snap) => store.applySnapshot(snap as Snapshot));
      li.appendChild(retry);
    } else if (!s.cards.length) {
      li.innerHTML =
        `<h3>No agents running</h3>` +
        `<p>Spawn a shell, Claude Code, Codex, or any CLI process — or paste one of these in a terminal:</p>` +
        `<pre class="terminal">herdr spawn --profile bash --cwd /tmp -- bash --norc -i\nherdr spawn --profile claude-code --cwd ~/project -- claude</pre>`;
      const spawn = document.createElement("button");
      spawn.textContent = "spawn agent";
      spawn.onclick = () => openSpawnModal(store, invoke);
      li.appendChild(spawn);
      const presets = document.createElement("div");
      presets.className = "preset-row";
      for (const [profile, label] of [
        ["bash", "Bash"],
        ["claude-code", "Claude Code"],
        ["codex", "Codex"],
        ["generic", "Generic"],
      ] as Array<[string, string]>) {
        const b = document.createElement("button");
        b.className = "ghost";
        b.textContent = label;
        b.onclick = () => openSpawnModal(store, invoke, { profile });
        presets.appendChild(b);
      }
      li.appendChild(presets);
      const copy = document.createElement("button");
      copy.className = "ghost";
      copy.textContent = "copy CLI example";
      copy.onclick = () => {
        navigator.clipboard.writeText("herdr spawn --profile bash --cwd /tmp -- bash --norc -i");
        copy.textContent = "copied";
      };
      li.appendChild(spawn);
      li.appendChild(copy);
    } else {
      li.innerHTML =
        `<h3>No agents match</h3><p>Adjust search, state filter, or host scope.</p>`;
    }
    list.appendChild(li);
  }

  document.addEventListener("keydown", (e: KeyboardEvent) => {
    if (/INPUT|SELECT|TEXTAREA/.test(document.activeElement?.tagName ?? "")) return;
    const visible = store.visibleAgents();
    if (!visible.length) return;
    if (e.key === "/") {
      e.preventDefault();
      search.focus();
      return;
    }
    let idx = visible.findIndex((c) => c.info.id === store.state.selectedId);
    const move = (d: number) => {
      idx = Math.min(visible.length - 1, Math.max(0, (idx < 0 ? 0 : idx) + d));
      onSelect(visible[idx].info.id);
    };
    if (e.key === "j" || e.key === "ArrowDown") move(1);
    else if (e.key === "k" || e.key === "ArrowUp") move(-1);
    else if (e.key === "Enter") req("composer").focus();
  });
}
