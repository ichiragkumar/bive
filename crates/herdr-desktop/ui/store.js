// UiStore mirror: single state owner for the webview. Event-driven — every
// mutation comes from a `herdr://event` push or a one-shot snapshot. Modules
// subscribe and re-render; nobody else holds fleet state.

import { isToolLine } from "./components/shared.js";

export const MIN_SENT_MATCH_CHARS = 3;

export function createStore() {
  const state = {
    cards: [],
    /** agent id → ChatSegment[] (authoritative from snapshot `chat`). */
    chat: {},
    /** [[RemoteHost, bool]] from remote_list_cmd. */
    hosts: [],
    selectedId: null,
    /** agent id → last tab ('chat' | 'terminal' | 'info'). */
    tabs: {},
    filter: "all",
    /** sidebar host selection; null = local. */
    selectedHost: null,
    connected: false,
    /** agent id → pending sent texts awaiting echo (live chat mirror). */
    pendingSent: {},
  };
  const listeners = new Set();
  const emit = () => { for (const fn of listeners) fn(state); };

  function findCard(id) {
    return state.cards.find((c) => c.info.id === id);
  }

  /** Append live output lines to the chat mirror (snapshot stays authoritative). */
  function appendChatLines(id, text) {
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
      const kind = hit >= 0 ? "Human" : isToolLine(line) ? "Tool" : "Agent";
      if (hit >= 0) pending.splice(hit, 1);
      const last = turns[turns.length - 1];
      if (last && last.kind === kind) last.text += "\n" + line;
      else turns.push({ kind, text: line });
    }
    if (turns.length > 500) turns.splice(0, turns.length - 500);
  }

  return {
    state,
    subscribe(fn) {
      listeners.add(fn);
      return () => listeners.delete(fn);
    },
    findCard,

    applySnapshot(snap) {
      if (!snap) return;
      state.cards = snap.cards || [];
      state.chat = snap.chat || {};
      state.pendingSent = {};
      if (state.selectedId && !findCard(state.selectedId)) state.selectedId = null;
      if (!state.selectedId && state.cards.length) state.selectedId = state.cards[0].info.id;
      emit();
    },

    applyHosts(hosts) {
      state.hosts = hosts || [];
      emit();
    },

    applyEvent(ev) {
      const kind = Object.keys(ev)[0];
      const body = ev[kind];
      const id = body.agent_id || (body.info && body.info.id);
      if (kind === "AgentSpawned") {
        if (!findCard(id)) state.cards.push({ info: body.info, log_tail: "", media: [] });
        else findCard(id).info = body.info;
        if (!state.selectedId) state.selectedId = id;
      } else if (kind === "AgentOutput") {
        const c = findCard(id);
        if (c) {
          c.log_tail = ((c.log_tail || "") + body.payload).slice(-64 * 1024);
          appendChatLines(id, body.payload);
        }
      } else if (kind === "AgentMedia") {
        const c = findCard(id);
        if (c) {
          c.media = c.media || [];
          c.media.push({ mime: body.mime, data_base64: body.data_base64, caption: body.caption });
          const line = `[media: ${body.mime}]\n`;
          c.log_tail = ((c.log_tail || "") + line).slice(-64 * 1024);
          appendChatLines(id, line);
        }
      } else if (kind === "StateChange") {
        const c = findCard(id);
        if (c) c.info.state = body.state;
      } else if (kind === "AgentExited") {
        const c = findCard(id);
        if (c) c.info.state = body.code === 0 ? { Exited: 0 } : { Errored: `exit ${body.code}` };
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
    noteSentLocal(id, text) {
      if (!text || text.length < MIN_SENT_MATCH_CHARS) return;
      (state.pendingSent[id] = state.pendingSent[id] || []).push(text);
    },

    select(id) {
      if (findCard(id)) {
        state.selectedId = id;
        emit();
      }
    },

    setTab(id, tab) {
      state.tabs[id] = tab;
      emit();
    },

    tabFor(id, profile) {
      const remembered = state.tabs[id];
      if (remembered) return remembered;
      // bash/generic have no turns worth chatting over: land on raw output.
      return profile === "bash" || profile === "generic" ? "terminal" : "chat";
    },

    setFilter(f) {
      state.filter = f;
      emit();
    },

    setConnected(c) {
      state.connected = c;
      emit();
    },
  };
}
