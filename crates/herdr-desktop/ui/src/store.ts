// UiStore mirror: single state owner for the webview. Event-driven — every
// mutation comes from a `herdr://event` push or a one-shot snapshot. Modules
// subscribe and re-render; nobody else holds fleet state.
//
// Derived data (visible agents, counts, attention order) lives in selectors
// here, not in components, so every surface agrees.

import { isToolLine, stateName } from "./components/shared.js";
import {
  summaryCounts as selectorsSummaryCounts,
  visibleAgents as selectorsVisibleAgents,
} from "./selectors.js";

export const MIN_SENT_MATCH_CHARS = 3;
const MAX_TIMELINE_PER_AGENT = 200;
const MAX_SPARK_BARS = 24;

export interface AgentInfo {
  id: string;
  profile: string;
  command: string;
  cwd: string;
  state: unknown;
  started_at_unix_ms: number;
  last_output_unix_ms: number;
  host: string | null;
  parent: string | null;
}

export interface MediaBlock {
  mime: string;
  data_base64: string;
  caption: string | null;
}

export interface AgentCard {
  info: AgentInfo;
  log_tail: string;
  media: MediaBlock[];
}

export type ChatKind = "Human" | "Agent" | "Tool";

export interface ChatSegment {
  kind: ChatKind;
  text: string;
}

export interface Snapshot {
  cards?: AgentCard[];
  chat?: Record<string, ChatSegment[]>;
  connected?: boolean;
}

export interface RemoteHost {
  name: string;
  ssh_target: string;
  port: number;
  user: string | null;
}

/** Mirrors Rust `RemoteHostList { hosts: Vec<(RemoteHost, bool)> }`. */
export type RemoteHostEntry = [RemoteHost, boolean];

/** Raw daemon event envelope, keyed by variant (narrowed per branch). */
export type DaemonEvent = Record<string, any>;

/** One Events-tab row: client-side normalization of daemon events + sends. */
export interface TimelineEntry {
  t: number;
  kind: "spawned" | "output" | "state" | "media" | "exited" | "removed" | "sent";
  text: string;
}

export type SortKey = "attention" | "newest" | "activity" | "state";

export interface SummaryCounts {
  total: number;
  working: number;
  blocked: number;
  errored: number;
  idle: number;
  exited: number;
}

export interface StoreState {
  cards: AgentCard[];
  /** agent id → ChatSegment[] (authoritative from snapshot `chat`). */
  chat: Record<string, ChatSegment[]>;
  /** agent id → Events-tab rows (client-side timeline, bounded). */
  timeline: Record<string, TimelineEntry[]>;
  /** agent id → recent output-byte buckets (row sparklines, bounded). */
  activity: Record<string, number[]>;
  /** [[RemoteHost, bool]] from remote_list_cmd. */
  hosts: RemoteHostEntry[];
  selectedId: string | null;
  /** agent id → last tab ('thread' | 'terminal' | 'events' | 'media' | 'info'). */
  tabs: Record<string, string>;
  filter: string;
  search: string;
  sort: SortKey;
  /** sidebar host selection; null = local. */
  selectedHost: string | null;
  connected: boolean;
  /** true while showing the last snapshot with the daemon down. */
  stale: boolean;
  /** terminal follow-tail (pause = frozen scrollback). */
  follow: boolean;
  /** agent id → pending sent texts awaiting echo (live chat mirror). */
  pendingSent: Record<string, string[]>;
  /** Optimistic spawn rows awaiting their AgentSpawned (reconciled, bounded). */
  pendingSpawns: Array<{ key: string; profile: string; host: string | null; at: number }>;
  /** Exited ids hidden via "clear exited" (pruned against the fleet on resync). */
  hidden: string[];
}

export type Store = ReturnType<typeof createStore>;

export function createStore() {
  const state: StoreState = {
    cards: [],
    chat: {},
    timeline: {},
    activity: {},
    hosts: [],
    selectedId: null,
    tabs: {},
    filter: "all",
    search: "",
    sort: "attention",
    selectedHost: null,
    connected: false,
    stale: false,
    follow: true,
    pendingSent: {},
    pendingSpawns: [],
    hidden: [],
  };
  const listeners = new Set<(s: StoreState) => void>();
  // rAF-batched emit: a burst of output events re-renders once per frame,
  // keeping the UI usable with fast streams and large fleets.
  let emitQueued = false;
  const emit = () => {
    if (emitQueued) return;
    emitQueued = true;
    const flush = () => {
      emitQueued = false;
      for (const fn of listeners) fn(state);
    };
    if (typeof requestAnimationFrame === "function") requestAnimationFrame(flush);
    else setTimeout(flush, 0);
  };

  function findCard(id: string): AgentCard | undefined {
    return state.cards.find((c) => c.info.id === id);
  }

  function recordTimeline(id: string, kind: TimelineEntry["kind"], text: string): void {
    const rows = (state.timeline[id] = state.timeline[id] || []);
    rows.push({ t: Date.now(), kind, text });
    if (rows.length > MAX_TIMELINE_PER_AGENT)
      rows.splice(0, rows.length - MAX_TIMELINE_PER_AGENT);
  }

  function recordActivity(id: string, bytes: number): void {
    const bars = (state.activity[id] = state.activity[id] || []);
    const last = bars[bars.length - 1];
    // Coalesce into the current bucket while it is small (cheap sparkline).
    if (last !== undefined && last < 512) bars[bars.length - 1] = last + bytes;
    else bars.push(bytes);
    if (bars.length > MAX_SPARK_BARS) bars.splice(0, bars.length - MAX_SPARK_BARS);
  }

  /** Append live output lines to the chat mirror (snapshot stays authoritative). */
  function appendChatLines(id: string, text: string): void {
    let turns = state.chat[id];
    if (!turns) {
      turns = state.chat[id] = [];
    }
    const pending = state.pendingSent[id] || [];
    for (const line of text.split("\n")) {
      if (!line) continue;
      const hit = pending.findIndex(
        (s) => s.length >= MIN_SENT_MATCH_CHARS && line.includes(s),
      );
      const kind: ChatKind = hit >= 0 ? "Human" : isToolLine(line) ? "Tool" : "Agent";
      if (hit >= 0) pending.splice(hit, 1);
      const last = turns[turns.length - 1];
      if (last && last.kind === kind) last.text += "\n" + line;
      else turns.push({ kind, text: line });
    }
    if (turns.length > 500) turns.splice(0, turns.length - 500);
  }

  return {
    state,
    subscribe(fn: (s: StoreState) => void): () => void {
      listeners.add(fn);
      return () => {
        listeners.delete(fn);
      };
    },
    findCard,

    applySnapshot(snap: Snapshot | null | undefined): void {
      if (!snap) return;
      state.cards = snap.cards || [];
      state.chat = snap.chat || {};
      state.pendingSent = {};
      state.stale = false;
      // Reconcile view-only state against the authoritative fleet.
      const ids = new Set(state.cards.map((c) => c.info.id));
      state.hidden = state.hidden.filter((id) => ids.has(id));
      const now = Date.now();
      state.pendingSpawns = state.pendingSpawns.filter((p) => now - p.at < 20000);
      if (state.selectedId && !findCard(state.selectedId)) state.selectedId = null;
      if (!state.selectedId && state.cards.length)
        state.selectedId = state.cards[0].info.id;
      emit();
    },

    applyHosts(hosts: RemoteHostEntry[] | null | undefined): void {
      state.hosts = hosts || [];
      emit();
    },

    /** Apply one daemon event. Returns UI notices (`spawned:<id>:<profile>`)
     *  the shell turns into toasts. */
    applyEvent(ev: DaemonEvent): string[] {
      const notices: string[] = [];
      const kind = Object.keys(ev)[0];
      const body = ev[kind];
      const id: string = body.agent_id || (body.info && body.info.id);
      if (kind === "AgentSpawned") {
        const info = body.info as AgentInfo;
        if (!findCard(id)) {
          state.cards.push({ info, log_tail: "", media: [] });
          recordTimeline(id, "spawned", `${info.profile} · ${info.command}`);
          // Optimistic-spawn reconciliation: the first unknown spawn consumes
          // the oldest pending row → auto-select + toast from the caller.
          if (state.pendingSpawns.length) {
            state.pendingSpawns.shift();
            state.selectedId = id;
            notices.push(`spawned:${id}:${info.profile}`);
          } else if (!state.selectedId) state.selectedId = id;
        } else {
          (findCard(id) as AgentCard).info = info;
          if (!state.selectedId) state.selectedId = id;
        }
      } else if (kind === "AgentOutput") {
        const payload = body.payload as string;
        const c = findCard(id);
        if (c) {
          c.log_tail = ((c.log_tail || "") + payload).slice(-64 * 1024);
          c.info.last_output_unix_ms = Date.now();
          recordActivity(id, payload.length);
          appendChatLines(id, payload);
          recordTimeline(id, "output", payload.slice(0, 120));
        }
      } else if (kind === "AgentMedia") {
        const c = findCard(id);
        if (c) {
          c.media = c.media || [];
          c.media.push({
            mime: body.mime as string,
            data_base64: body.data_base64 as string,
            caption: (body.caption as string | null) ?? null,
          });
          const line = `[media: ${body.mime as string}]\n`;
          c.log_tail = ((c.log_tail || "") + line).slice(-64 * 1024);
          appendChatLines(id, line);
          recordTimeline(id, "media", `${body.mime as string}${body.caption ? ` — ${body.caption as string}` : ""}`);
        }
      } else if (kind === "StateChange") {
        const c = findCard(id);
        if (c) {
          c.info.state = body.state;
          recordTimeline(id, "state", `→ ${stateName(body.state)}`);
        }
      } else if (kind === "AgentExited") {
        const c = findCard(id);
        if (c) {
          c.info.state =
            body.code === 0 ? { Exited: 0 } : { Errored: `exit ${body.code as number}` };
          recordTimeline(id, "exited", `code ${body.code as number}`);
        }
      } else if (kind === "AgentRemoved") {
        recordTimeline(id, "removed", "removed from fleet");
        delete state.timeline[id];
        state.cards = state.cards.filter((c) => c.info.id !== id);
        delete state.chat[id];
        delete state.activity[id];
        state.hidden = state.hidden.filter((h) => h !== id);
        if (state.selectedId === id) {
          state.selectedId = state.cards.length ? state.cards[0].info.id : null;
        }
      }
      emit();
      return notices;
    },

    /** Queue a composer submission so its echo classifies as a human turn.
     *  Queue-only (no optimistic append): matches the Rust rule where the
     *  echoed line itself becomes the Human segment on arrival. */
    noteSentLocal(id: string, text: string): void {
      recordTimeline(id, "sent", text.slice(0, 120));
      if (!text || text.length < MIN_SENT_MATCH_CHARS) return;
      (state.pendingSent[id] = state.pendingSent[id] || []).push(text);
    },
    /** Replace a log tail (full-buffer load); chat/timeline keep appending. */
    setLogTail(id: string, text: string): void {
      const c = findCard(id);
      if (c) {
        c.log_tail = text.slice(-256 * 1024);
        emit();
      }
    },

    /** Optimistic spawn row (J1): rendered pulsing until AgentSpawned lands
     *  (reconciled there) or a snapshot drops it after 20 s. */
    noteSpawnOptimistic(profile: string, host: string | null): void {
      state.pendingSpawns.push({
        key: `pending-${Date.now()}-${state.pendingSpawns.length}`,
        profile,
        host,
        at: Date.now(),
      });
      if (state.pendingSpawns.length > 5) state.pendingSpawns.shift();
      emit();
    },

    /** Hide exited/errored cards locally ("clear exited"). View-only: the
     *  daemon still owns them until auto-prune; resync resurrects survivors
     *  only if they still exist (pruned in applySnapshot). */
    hideExited(): number {
      let n = 0;
      for (const c of state.cards) {
        const s = stateName(c.info.state);
        if ((s === "Exited" || s === "Errored") && !state.hidden.includes(c.info.id)) {
          state.hidden.push(c.info.id);
          n += 1;
        }
      }
      if (state.selectedId && state.hidden.includes(state.selectedId)) {
        const rest = state.cards.filter((c) => !state.hidden.includes(c.info.id));
        state.selectedId = rest.length ? rest[0].info.id : null;
      }
      if (n) emit();
      return n;
    },

    clearSelection(): void {
      state.selectedId = null;
      emit();
    },

    /** View-clock tick (relative times); no daemon traffic. */
    touch(): void {
      emit();
    },

    select(id: string): void {
      if (findCard(id)) {
        state.selectedId = id;
        emit();
      }
    },

    setTab(id: string, tab: string): void {
      state.tabs[id] = tab;
      emit();
    },

    tabFor(id: string, profile: string): string {
      const remembered = state.tabs[id];
      if (remembered) return remembered;
      // bash/generic have no turns worth chatting over: land on raw output.
      return profile === "bash" || profile === "generic" ? "terminal" : "thread";
    },

    setFilter(f: string): void {
      state.filter = f;
      emit();
    },

    setSearch(q: string): void {
      state.search = q;
      emit();
    },

    setSort(sort: SortKey): void {
      state.sort = sort;
      emit();
    },

    setFollow(f: boolean): void {
      state.follow = f;
      emit();
    },

    setConnected(c: boolean): void {
      state.connected = c;
      if (c) state.stale = false;
      else if (state.cards.length) state.stale = true;
      emit();
    },

    // ---- derived selectors (see selectors.ts; methods delegate) ----

    summaryCounts(): SummaryCounts {
      return selectorsSummaryCounts(state);
    },

    /** Fleet rows for filter + host scope + search + sort. Cleared
     *  (hidden) ids are excluded. */
    visibleAgents(): AgentCard[] {
      return selectorsVisibleAgents(state);
    },
  };
}
