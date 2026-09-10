// crates/herdr-desktop/ui/src/components/shared.ts
function req(id) {
  const el = document.getElementById(id);
  if (!el) throw new Error(`herdr UI: missing element #${id}`);
  return el;
}
function esc(s) {
  return s.replace(
    /[&<>"]/g,
    (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]
  );
}
function ansiToHtml(text) {
  const codes = { 30: "#6b7280", 31: "#e05656", 32: "#3fb96f", 33: "#e5b34a", 34: "#4c8dff", 35: "#c084fc", 36: "#22d3ee", 37: "#e6e9f0", 90: "#8b93a5" };
  let out = "";
  let color = null;
  for (const part of text.split(/\x1b\[/)) {
    const m = part.match(/^([\d;]+)m/);
    if (!m) {
      out += esc(part);
      continue;
    }
    const rest = part.slice(m[0].length);
    for (const code of m[1].split(";").map(Number)) {
      if (code === 0) color = null;
      else if (codes[code]) color = codes[code];
    }
    out += (color ? `<span style="color:${color}">` : "<span>") + esc(rest) + "</span>";
  }
  return out;
}
function stateName(state) {
  if (typeof state === "string") return state;
  if (typeof state === "object" && state !== null) {
    const obj = state;
    if ("Errored" in obj) return "Errored";
    if ("Exited" in obj) return "Exited";
    const keys = Object.keys(obj);
    if (keys.length) return keys[0];
  }
  return "Unknown";
}
function lastLine(text) {
  const clean = text.replace(/\x1b\[[?0-9;]*[a-zA-Z]/g, "");
  const lines = clean.trimEnd().split("\n");
  return lines[lines.length - 1] || "";
}
function isToolLine(line) {
  const t = line.trimStart();
  return t.startsWith("\u23FA") || t.startsWith("\u23BF") || line.includes("[media:");
}

// crates/herdr-desktop/ui/src/store.ts
var MIN_SENT_MATCH_CHARS = 3;
function createStore() {
  const state = {
    cards: [],
    chat: {},
    hosts: [],
    selectedId: null,
    tabs: {},
    filter: "all",
    selectedHost: null,
    connected: false,
    pendingSent: {}
  };
  const listeners = /* @__PURE__ */ new Set();
  const emit = () => {
    for (const fn of listeners) fn(state);
  };
  function findCard(id) {
    return state.cards.find((c) => c.info.id === id);
  }
  function appendChatLines(id, text) {
    let turns = state.chat[id];
    if (!turns) {
      turns = state.chat[id] = [];
    }
    const pending = state.pendingSent[id] || [];
    for (const line of text.split("\n")) {
      if (!line) continue;
      const hit = pending.findIndex(
        (s) => s.length >= MIN_SENT_MATCH_CHARS && line.includes(s)
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
      return () => {
        listeners.delete(fn);
      };
    },
    findCard,
    applySnapshot(snap) {
      if (!snap) return;
      state.cards = snap.cards || [];
      state.chat = snap.chat || {};
      state.pendingSent = {};
      if (state.selectedId && !findCard(state.selectedId)) state.selectedId = null;
      if (!state.selectedId && state.cards.length)
        state.selectedId = state.cards[0].info.id;
      emit();
    },
    applyHosts(hosts) {
      state.hosts = hosts || [];
      emit();
    },
    applyEvent(ev) {
      const kind = Object.keys(ev)[0];
      const body = ev[kind];
      const id = body.agent_id || body.info && body.info.id;
      if (kind === "AgentSpawned") {
        if (!findCard(id))
          state.cards.push({ info: body.info, log_tail: "", media: [] });
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
          c.media.push({
            mime: body.mime,
            data_base64: body.data_base64,
            caption: body.caption ?? null
          });
          const line = `[media: ${body.mime}]
`;
          c.log_tail = ((c.log_tail || "") + line).slice(-64 * 1024);
          appendChatLines(id, line);
        }
      } else if (kind === "StateChange") {
        const c = findCard(id);
        if (c) c.info.state = body.state;
      } else if (kind === "AgentExited") {
        const c = findCard(id);
        if (c)
          c.info.state = body.code === 0 ? { Exited: 0 } : { Errored: `exit ${body.code}` };
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
      return profile === "bash" || profile === "generic" ? "terminal" : "chat";
    },
    setFilter(f) {
      state.filter = f;
      emit();
    },
    setConnected(c) {
      state.connected = c;
      emit();
    }
  };
}

// crates/herdr-desktop/ui/src/components/topbar.ts
function fleetDot(cards) {
  if (!cards.length) return "gray";
  if (cards.some((c) => stateName(c.info.state) === "Errored")) return "red";
  if (cards.some((c) => stateName(c.info.state) === "Blocked")) return "amber";
  return "green";
}
function mountTopbar(store2, invoke2) {
  const connDot = req("conn-dot");
  const fleetDotEl = req("fleet-dot");
  const summary = req("fleet-summary");
  req("btn-kill-all").onclick = () => invoke2("kill_all_cmd");
  req("btn-side").onclick = () => document.body.classList.toggle("no-side");
  store2.subscribe((s) => {
    connDot.classList.toggle("connected", s.connected);
    const counts = { Working: 0, Blocked: 0, Errored: 0 };
    for (const c of s.cards) {
      const n = stateName(c.info.state);
      if (n in counts) counts[n] += 1;
    }
    const total = s.cards.length;
    summary.textContent = total ? `${total} agent${total === 1 ? "" : "s"} \u2014 ${counts["Working"]} working, ${counts["Blocked"]} blocked, ${counts["Errored"]} errored` : "no agents";
    fleetDotEl.className = "dot " + fleetDot(s.cards);
  });
}

// crates/herdr-desktop/ui/src/components/tray.ts
function mountTray(store2) {
  store2.subscribe((s) => {
    const dot = fleetDot(s.cards);
    const blocked = s.cards.filter((c) => stateName(c.info.state) === "Blocked").length;
    const base = "herdr \u2014 agent fleet";
    document.title = blocked ? `${base} (${blocked} need input)` : base;
    document.title += dot === "gray" ? "" : ` [${dot}]`;
  });
}

// crates/herdr-desktop/ui/src/components/sidebar.ts
function mountSidebar(store2, invoke2) {
  const hostList = req("host-list");
  const profilePicker = req("profile-picker");
  const filterSel = req("filter");
  req("btn-spawn").onclick = async () => {
    await invoke2("spawn_agent_cmd", {
      profile: profilePicker.value,
      cwd: "/tmp",
      command: window.__HERDR_SHELL__ || "/bin/bash",
      args: [],
      host: store2.state.selectedHost
    });
  };
  req("btn-add-remote").onclick = async () => {
    const name = window.prompt("Remote name (used with spawn --host):");
    if (!name) return;
    const sshTarget = window.prompt(`SSH target for "${name}" (host or user@host):`);
    if (!sshTarget) return;
    await invoke2("remote_add_cmd", { name, sshTarget, port: 22, user: null });
    refreshHosts();
  };
  async function refreshHosts() {
    try {
      const hosts = await invoke2("remote_list_cmd");
      store2.applyHosts(hosts);
    } catch {
    }
  }
  refreshHosts();
  let lastFleetKey = "";
  store2.subscribe((s) => {
    const key = s.cards.map((c) => c.info.id).join(",");
    if (key !== lastFleetKey) {
      lastFleetKey = key;
      refreshHosts();
    }
    renderHosts();
  });
  hostList.onclick = async (e) => {
    const li = e.target?.closest("li[data-host]");
    if (!li) return;
    if (e.target?.closest("button.rm-host")) {
      await invoke2("remote_remove_cmd", { name: li.dataset.host });
      if (store2.state.selectedHost === li.dataset.host) store2.state.selectedHost = null;
      refreshHosts();
      return;
    }
    store2.state.selectedHost = li.dataset.host === "" ? null : li.dataset.host;
    renderHosts();
  };
  filterSel.onchange = () => store2.setFilter(filterSel.value);
  function renderHosts() {
    const s = store2.state;
    const seen = /* @__PURE__ */ new Map();
    for (const c of s.cards) {
      const h = c.info.host || null;
      seen.set(h, (seen.get(h) || 0) + 1);
    }
    const rows = [["", "Local", seen.get(null) || 0]];
    for (const [host, up] of s.hosts) {
      rows.push([
        host.name,
        up ? esc(host.name) : `${esc(host.name)} (offline)`,
        seen.get(host.name) || 0
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
      if ((store2.state.selectedHost || "") === key) li.classList.add("selected");
      li.innerHTML = `<span class="host-name">${label}</span><span class="count">${count}</span>`;
      if (key !== "") {
        const rm = document.createElement("button");
        rm.className = "rm-host ghost";
        rm.title = `forget host ${key}`;
        rm.textContent = "\xD7";
        li.appendChild(rm);
      }
      hostList.appendChild(li);
    }
  }
}

// crates/herdr-desktop/ui/src/components/agent-list.ts
function mountAgentList(store2, onSelect) {
  const list = req("agent-list");
  function visibleInScope() {
    const s = store2.state;
    return s.cards.filter(
      (c) => (s.filter === "all" || stateName(c.info.state) === s.filter) && (s.selectedHost === null || (c.info.host || null) === s.selectedHost)
    );
  }
  store2.subscribe((s) => renderList(visibleInScope(), s.selectedId));
  function renderList(visible, selectedId) {
    list.innerHTML = "";
    if (!visible.length) {
      const li = document.createElement("li");
      li.className = "empty-row";
      li.textContent = "No agents. Spawn one from the sidebar or run herdr spawn.";
      list.appendChild(li);
      return;
    }
    for (const card of visible) {
      const li = document.createElement("li");
      li.className = "agent-row";
      if (card.info.id === selectedId) li.classList.add("selected");
      const st = stateName(card.info.state);
      li.innerHTML = `<span class="id">${esc(card.info.id.slice(0, 12))}</span><span class="profile">${esc(card.info.profile)}</span><span class="badge ${esc(st)}">${esc(st)}</span><span class="last-line">${esc(lastLine(card.log_tail || "")) || "&nbsp;"}</span>`;
      li.onclick = () => onSelect(card.info.id);
      list.appendChild(li);
    }
  }
  document.addEventListener("keydown", (e) => {
    if (/INPUT|SELECT|TEXTAREA/.test(document.activeElement?.tagName ?? "")) return;
    const visible = visibleInScope();
    if (!visible.length) return;
    let idx = visible.findIndex((c) => c.info.id === store2.state.selectedId);
    const move = (d) => {
      idx = Math.min(visible.length - 1, Math.max(0, (idx < 0 ? 0 : idx) + d));
      onSelect(visible[idx].info.id);
    };
    if (e.key === "j" || e.key === "ArrowDown") move(1);
    else if (e.key === "k" || e.key === "ArrowUp") move(-1);
    else if (e.key === "Enter") req("composer").focus();
  });
}

// crates/herdr-desktop/ui/src/components/detail.ts
function mountDetail(store2, invoke2) {
  const title = req("detail-title");
  const tabs = Array.from(document.querySelectorAll("#detail-header .tab"));
  const panels = {
    chat: req("tab-chat"),
    terminal: req("tab-terminal"),
    info: req("tab-info")
  };
  tabs.forEach((btn) => {
    btn.onclick = () => {
      const id = store2.state.selectedId;
      if (id) store2.setTab(id, btn.dataset["tab"]);
      else showTab("chat");
    };
  });
  req("detail-kill").onclick = async () => {
    const id = store2.state.selectedId;
    if (id) await invoke2("kill_agent_cmd", { agentId: id });
  };
  req("detail-copy").onclick = () => {
    const id = store2.state.selectedId;
    if (id) navigator.clipboard.writeText(id);
  };
  function showTab(tab) {
    tabs.forEach((b) => b.classList.toggle("active", b.dataset["tab"] === tab));
    for (const [name, el] of Object.entries(panels)) {
      el.hidden = name !== tab;
    }
  }
  store2.subscribe((s) => {
    const card = s.cards.find((c) => c.info.id === s.selectedId);
    if (!card) {
      title.textContent = "no agent selected";
      return;
    }
    title.textContent = `${card.info.id.slice(0, 12)} \xB7 ${card.info.profile} \xB7 ${card.info.command}`;
    showTab(store2.tabFor(card.info.id, card.info.profile));
  });
}

// crates/herdr-desktop/ui/src/components/detail-chat.ts
var RAW_PROFILES = /* @__PURE__ */ new Set(["bash", "generic"]);
function mountDetailChat(store2) {
  const panel = req("tab-chat");
  const turnsEl = req("chat-turns");
  const mediaEl = req("chat-media");
  store2.subscribe((s) => {
    const card = s.cards.find((c) => c.info.id === s.selectedId);
    if (!card || store2.tabFor(card.info.id, card.info.profile) !== "chat") return;
    renderChat(card, s.chat[card.info.id] || []);
  });
  function renderChat(card, turns) {
    mediaEl.innerHTML = "";
    for (const m of card.media || []) {
      const fig = document.createElement("figure");
      const img = document.createElement("img");
      img.src = `data:${m.mime};base64,${m.data_base64}`;
      if (m.caption) {
        const cap = document.createElement("figcaption");
        cap.textContent = m.caption;
        fig.appendChild(cap);
      }
      fig.prepend(img);
      mediaEl.appendChild(fig);
    }
    turnsEl.innerHTML = "";
    if (RAW_PROFILES.has(card.info.profile)) {
      turnsEl.innerHTML = `<div class="chat-notice">Raw output only for <b>${esc(card.info.profile)}</b> \u2014 see the Terminal tab. Chat turns are parsed for claude-code / codex agents.</div>`;
      return;
    }
    if (!turns.length) {
      turnsEl.innerHTML = `<div class="chat-notice">No turns yet \u2014 output will appear here.</div>`;
      return;
    }
    for (const t of turns) {
      const div = document.createElement("div");
      div.className = `turn ${t.kind.toLowerCase()}`;
      const who = t.kind === "Human" ? "you" : t.kind === "Tool" ? "tool" : "agent";
      div.innerHTML = `<span class="who">${esc(who)}</span><pre>${esc(t.text)}</pre>`;
      turnsEl.appendChild(div);
    }
    panel.scrollTop = panel.scrollHeight;
  }
}

// crates/herdr-desktop/ui/src/components/detail-terminal.ts
function mountDetailTerminal(store2) {
  const panel = req("tab-terminal");
  const logEl = req("detail-log");
  store2.subscribe((s) => {
    const card = s.cards.find((c) => c.info.id === s.selectedId);
    if (!card || store2.tabFor(card.info.id, card.info.profile) !== "terminal") return;
    logEl.innerHTML = ansiToHtml(card.log_tail || "");
    panel.scrollTop = panel.scrollHeight;
  });
}

// crates/herdr-desktop/ui/src/components/detail-info.ts
function uptime(startedMs) {
  const s = Math.max(0, Math.floor((Date.now() - startedMs) / 1e3));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`;
  return `${Math.floor(s / 3600)}h ${Math.floor(s % 3600 / 60)}m`;
}
function mountDetailInfo(store2) {
  const list = req("detail-info-list");
  store2.subscribe((s) => {
    const card = s.cards.find((c) => c.info.id === s.selectedId);
    if (!card || store2.tabFor(card.info.id, card.info.profile) !== "info") return;
    const info = card.info;
    const rows = [
      ["id", info.id],
      ["host", info.host || "local"],
      ["profile", info.profile],
      ["cwd", info.cwd],
      ["command", info.command],
      ["state", stateName(info.state)],
      ["uptime", uptime(info.started_at_unix_ms)],
      ["media blocks", String((card.media || []).length)]
    ];
    list.innerHTML = "";
    for (const [k, v] of rows) {
      const dt = document.createElement("dt");
      dt.textContent = k;
      const dd = document.createElement("dd");
      dd.textContent = v;
      list.appendChild(dt);
      list.appendChild(dd);
    }
  });
}

// crates/herdr-desktop/ui/src/components/composer.ts
function mountComposer(store2, invoke2) {
  const input = req("composer");
  req("composer-send").onclick = sendLine;
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") sendLine();
  });
  async function sendLine() {
    const id = store2.state.selectedId;
    if (!id || !input.value) return;
    const text = input.value;
    input.value = "";
    await invoke2("send_input_cmd", { agentId: id, text, raw: false });
    store2.noteSentLocal(id, text);
  }
}

// crates/herdr-desktop/ui/src/main.ts
function fatal(msg) {
  const el = req("fatal");
  el.hidden = false;
  el.textContent = msg;
}
if (!window.__TAURI__) {
  fatal(
    "herdr UI needs its host: open it in the Tauri app (cargo run -p herdr-desktop --features tauri) or view the static preview (python3 scripts/make_preview.py, then target/preview/index.html)."
  );
  throw new Error("missing window.__TAURI__");
}
var { invoke } = window.__TAURI__.core;
var { listen } = window.__TAURI__.event;
var store = createStore();
mountTopbar(store, invoke);
mountTray(store);
mountSidebar(store, invoke);
mountAgentList(store, (id) => store.select(id));
mountDetail(store, invoke);
mountDetailChat(store);
mountDetailTerminal(store);
mountDetailInfo(store);
mountComposer(store, invoke);
await listen("herdr://event", ({ payload }) => {
  const ev = payload && (payload.event || payload);
  store.applyEvent(ev);
});
await listen("herdr://conn", ({ payload }) => {
  const connected = payload === "connected";
  store.setConnected(connected);
  if (connected) invoke("snapshot_cmd").then((snap) => store.applySnapshot(snap));
});
invoke("snapshot_cmd").then((snap) => store.applySnapshot(snap)).catch(() => {
});
window.__HERDR_APP__ = {
  get cards() {
    return store.state.cards;
  },
  get store() {
    return store;
  },
  applySnapshot: (snap) => store.applySnapshot(snap)
};
