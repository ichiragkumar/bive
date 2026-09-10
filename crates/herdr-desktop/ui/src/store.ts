// UiStore mirror: single state owner for the webview. Event-driven — every
// mutation comes from a `herdr://event` push or a one-shot snapshot. Modules
// subscribe and re-render; nobody else holds fleet state.

import { isToolLine } from "./components/shared.js";

export const MIN_SENT_MATCH_CHARS = 3;

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

export interface StoreState {
  cards: AgentCard[];
  /** agent id → ChatSegment[] (authoritative from snapshot `chat`). */
  chat: Record<string, ChatSegment[]>;
  /** [[RemoteHost, bool]] from remote_list_cmd. */
  hosts: RemoteHostEntry[];
  selectedId: string | null;
  /** agent id → last tab ('chat' | 'terminal' | 'info'). */
  tabs: Record<string, string>;
  filter: string;
  /** sidebar host selection; null = local. */
  selectedHost: string | null;
  connected: boolean;
  /** agent id → pending sent texts awaiting echo (live chat mirror). */
  pendingSent: Record<string, string[]>;
}

export type Store = ReturnType<typeof createStore>;

export function createStore() {
  const state: StoreState = {
    cards: [],
    chat: {},
    hosts: [],
    selectedId: null,
    tabs: {},
    filter: "all",
    selectedHost: null,
    connected: false,
    pendingSent: {},
  };
  const listeners = new Set<(s: StoreState) => void>();
  const emit = () => {
    for (const fn of listeners) fn(state);
  };

  function findCard(id: string): AgentCard | undefined {
    return state.cards.find((c) => c.info.id === id);
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
      if (state.selectedId && !findCard(state.selectedId)) state.selectedId = null;
      if (!state.selectedId && state.cards.length)
        state.selectedId = state.cards[0].info.id;
      emit();
    },

    applyHosts(hosts: RemoteHostEntry[] | null | undefined): void {
      state.hosts = hosts || [];
      emit();
    },

    applyEvent(ev: DaemonEvent): void {
      const kind = Object.keys(ev)[0];
      const body = ev[kind];
      const id: string = body.agent_id || (body.info && body.info.id);
      if (kind === "AgentSpawned") {
        if (!findCard(id))
          state.cards.push({ info: body.info as AgentInfo, log_tail: "", media: [] });
        else (findCard(id) as AgentCard).info = body.info as AgentInfo;
        if (!state.selectedId) state.selectedId = id;
      } else if (kind === "AgentOutput") {
        const c = findCard(id);
        if (c) {
          c.log_tail = ((c.log_tail || "") + (body.payload as string)).slice(-64 * 1024);
          appendChatLines(id, body.payload as string);
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
        }
      } else if (kind === "StateChange") {
        const c = findCard(id);
        if (c) c.info.state = body.state;
      } else if (kind === "AgentExited") {
        const c = findCard(id);
        if (c)
          c.info.state =
            body.code === 0 ? { Exited: 0 } : { Errored: `exit ${body.code as number}` };
      } else if (kind === "AgentRemoved") {
        state.cards = state.cards.filter((c) => c.info.id !== id);
        delete state.chat[id];
        if (state.selectedId === id) {
          state.selectedId = state.cards.length ? state.cards[0].info.id : null;
        }
      }
      emit();
    },

    /** Queue a composer submission so its echo classifies as a human turn.
     *  Queue-only (no optimistic append): matches the Rust rule where the
     *  echoed line itself becomes the Human segment on arrival. */
    noteSentLocal(id: string, text: string): void {
      if (!text || text.length < MIN_SENT_MATCH_CHARS) return;
      (state.pendingSent[id] = state.pendingSent[id] || []).push(text);
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
      return profile === "bash" || profile === "generic" ? "terminal" : "chat";
    },

    setFilter(f: string): void {
      state.filter = f;
      emit();
    },

    setConnected(c: boolean): void {
      state.connected = c;
      emit();
    },
  };
}
