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
  const lines = stripAnsi(text).trimEnd().split("\n");
  return lines[lines.length - 1] || "";
}
function stripAnsi(text) {
  return text.replace(/\x1b\[[?0-9;]*[a-zA-Z]/g, "");
}
function isToolLine(line) {
  const t = line.trimStart();
  return t.startsWith("\u23FA") || t.startsWith("\u23BF") || line.includes("[media:");
}

// crates/herdr-desktop/ui/src/selectors.ts
function attentionRank(state) {
  const n = stateName(state);
  if (n === "Errored") return 0;
  if (n === "Blocked") return 1;
  if (n === "Working" || n === "Starting") return 2;
  if (n === "Idle") return 3;
  return 4;
}
function summaryCounts(state) {
  const counts = {
    total: state.cards.length,
    working: 0,
    blocked: 0,
    errored: 0,
    idle: 0,
    exited: 0
  };
  for (const c of state.cards) {
    const n = stateName(c.info.state);
    if (n === "Working" || n === "Starting") counts.working += 1;
    else if (n === "Blocked") counts.blocked += 1;
    else if (n === "Errored") counts.errored += 1;
    else if (n === "Idle") counts.idle += 1;
    else counts.exited += 1;
  }
  return counts;
}
function visibleAgents(state) {
  const q = state.search.trim().toLowerCase();
  const rows = state.cards.filter((c) => {
    if (state.hidden.includes(c.info.id)) return false;
    if (state.filter === "attention") {
      const n = stateName(c.info.state);
      if (n !== "Blocked" && n !== "Errored") return false;
    } else if (state.filter !== "all" && stateName(c.info.state) !== state.filter) {
      return false;
    }
    if (state.selectedHost !== null && (c.info.host || null) !== state.selectedHost)
      return false;
    if (q) {
      const hay = `${c.info.id} ${c.info.profile} ${c.info.command} ${c.info.cwd} ${c.info.host || "local"}`.toLowerCase();
      if (!hay.includes(q)) return false;
    }
    return true;
  });
  const by = {
    attention: (a, b) => attentionRank(a.info.state) - attentionRank(b.info.state) || b.info.last_output_unix_ms - a.info.last_output_unix_ms,
    newest: (a, b) => b.info.started_at_unix_ms - a.info.started_at_unix_ms,
    activity: (a, b) => b.info.last_output_unix_ms - a.info.last_output_unix_ms,
    state: (a, b) => stateName(a.info.state).localeCompare(stateName(b.info.state)) || b.info.last_output_unix_ms - a.info.last_output_unix_ms
  };
  return rows.sort(by[state.sort]);
}

// crates/herdr-desktop/ui/src/store.ts
var MIN_SENT_MATCH_CHARS = 3;
var MAX_TIMELINE_PER_AGENT = 200;
var MAX_SPARK_BARS = 24;
function createStore() {
  const state = {
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
    hidden: []
  };
  const listeners = /* @__PURE__ */ new Set();
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
  function findCard(id) {
    return state.cards.find((c) => c.info.id === id);
  }
  function recordTimeline(id, kind, text) {
    const rows = state.timeline[id] = state.timeline[id] || [];
    rows.push({ t: Date.now(), kind, text });
    if (rows.length > MAX_TIMELINE_PER_AGENT)
      rows.splice(0, rows.length - MAX_TIMELINE_PER_AGENT);
  }
  function recordActivity(id, bytes) {
    const bars = state.activity[id] = state.activity[id] || [];
    const last = bars[bars.length - 1];
    if (last !== void 0 && last < 512) bars[bars.length - 1] = last + bytes;
    else bars.push(bytes);
    if (bars.length > MAX_SPARK_BARS) bars.splice(0, bars.length - MAX_SPARK_BARS);
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
      state.stale = false;
      const ids = new Set(state.cards.map((c) => c.info.id));
      state.hidden = state.hidden.filter((id) => ids.has(id));
      const now = Date.now();
      state.pendingSpawns = state.pendingSpawns.filter((p) => now - p.at < 2e4);
      if (state.selectedId && !findCard(state.selectedId)) state.selectedId = null;
      if (!state.selectedId && state.cards.length)
        state.selectedId = state.cards[0].info.id;
      emit();
    },
    applyHosts(hosts) {
      state.hosts = hosts || [];
      emit();
    },
    /** Apply one daemon event. Returns UI notices (`spawned:<id>:<profile>`)
     *  the shell turns into toasts. */
    applyEvent(ev) {
      const notices = [];
      const kind = Object.keys(ev)[0];
      const body = ev[kind];
      const id = body.agent_id || body.info && body.info.id;
      if (kind === "AgentSpawned") {
        const info = body.info;
        if (!findCard(id)) {
          state.cards.push({ info, log_tail: "", media: [] });
          recordTimeline(id, "spawned", `${info.profile} \xB7 ${info.command}`);
          if (state.pendingSpawns.length) {
            state.pendingSpawns.shift();
            state.selectedId = id;
            notices.push(`spawned:${id}:${info.profile}`);
          } else if (!state.selectedId) state.selectedId = id;
        } else {
          findCard(id).info = info;
          if (!state.selectedId) state.selectedId = id;
        }
      } else if (kind === "AgentOutput") {
        const payload = body.payload;
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
            mime: body.mime,
            data_base64: body.data_base64,
            caption: body.caption ?? null
          });
          const line = `[media: ${body.mime}]
`;
          c.log_tail = ((c.log_tail || "") + line).slice(-64 * 1024);
          appendChatLines(id, line);
          recordTimeline(id, "media", `${body.mime}${body.caption ? ` \u2014 ${body.caption}` : ""}`);
        }
      } else if (kind === "StateChange") {
        const c = findCard(id);
        if (c) {
          c.info.state = body.state;
          recordTimeline(id, "state", `\u2192 ${stateName(body.state)}`);
        }
      } else if (kind === "AgentExited") {
        const c = findCard(id);
        if (c) {
          c.info.state = body.code === 0 ? { Exited: 0 } : { Errored: `exit ${body.code}` };
          recordTimeline(id, "exited", `code ${body.code}`);
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
    noteSentLocal(id, text) {
      recordTimeline(id, "sent", text.slice(0, 120));
      if (!text || text.length < MIN_SENT_MATCH_CHARS) return;
      (state.pendingSent[id] = state.pendingSent[id] || []).push(text);
    },
    /** Replace a log tail (full-buffer load); chat/timeline keep appending. */
    setLogTail(id, text) {
      const c = findCard(id);
      if (c) {
        c.log_tail = text.slice(-256 * 1024);
        emit();
      }
    },
    /** Optimistic spawn row (J1): rendered pulsing until AgentSpawned lands
     *  (reconciled there) or a snapshot drops it after 20 s. */
    noteSpawnOptimistic(profile, host) {
      state.pendingSpawns.push({
        key: `pending-${Date.now()}-${state.pendingSpawns.length}`,
        profile,
        host,
        at: Date.now()
      });
      if (state.pendingSpawns.length > 5) state.pendingSpawns.shift();
      emit();
    },
    /** Hide exited/errored cards locally ("clear exited"). View-only: the
     *  daemon still owns them until auto-prune; resync resurrects survivors
     *  only if they still exist (pruned in applySnapshot). */
    hideExited() {
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
    clearSelection() {
      state.selectedId = null;
      emit();
    },
    /** View-clock tick (relative times); no daemon traffic. */
    touch() {
      emit();
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
      return profile === "bash" || profile === "generic" ? "terminal" : "thread";
    },
    setFilter(f) {
      state.filter = f;
      emit();
    },
    setSearch(q) {
      state.search = q;
      emit();
    },
    setSort(sort) {
      state.sort = sort;
      emit();
    },
    setFollow(f) {
      state.follow = f;
      emit();
    },
    setConnected(c) {
      state.connected = c;
      if (c) state.stale = false;
      else if (state.cards.length) state.stale = true;
      emit();
    },
    // ---- derived selectors (see selectors.ts; methods delegate) ----
    summaryCounts() {
      return summaryCounts(state);
    },
    /** Fleet rows for filter + host scope + search + sort. Cleared
     *  (hidden) ids are excluded. */
    visibleAgents() {
      return visibleAgents(state);
    }
  };
}

// crates/herdr-desktop/ui/src/components/modal.ts
function openModalShell(title) {
  const root = req("modal-root");
  root.innerHTML = "";
  root.hidden = false;
  const overlay = document.createElement("div");
  overlay.className = "modal-overlay";
  const dialog = document.createElement("div");
  dialog.className = "modal";
  dialog.setAttribute("role", "dialog");
  dialog.setAttribute("aria-label", title);
  const h = document.createElement("h2");
  h.textContent = title;
  dialog.appendChild(h);
  overlay.appendChild(dialog);
  root.appendChild(overlay);
  const prevFocus = document.activeElement;
  const close = () => {
    root.hidden = true;
    root.innerHTML = "";
    document.removeEventListener("keydown", onKey, true);
    prevFocus?.focus?.();
  };
  const onKey = (e) => {
    if (e.key === "Escape") {
      e.stopPropagation();
      close();
    }
    if (e.key === "Tab") {
      const focusables = Array.from(
        dialog.querySelectorAll("button, input, select, textarea, [tabindex]")
      ).filter((el) => !el.disabled);
      if (!focusables.length) return;
      const first = focusables[0];
      const last = focusables[focusables.length - 1];
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    }
  };
  document.addEventListener("keydown", onKey, true);
  overlay.addEventListener("mousedown", (e) => {
    if (e.target === overlay) close();
  });
  return { root: dialog, close };
}
function confirmModal(opts) {
  return new Promise((resolve) => {
    const { root, close } = openModalShell(opts.title);
    const p = document.createElement("p");
    p.textContent = opts.body;
    const row = document.createElement("div");
    row.className = "modal-actions";
    const cancel = document.createElement("button");
    cancel.textContent = "cancel";
    const ok = document.createElement("button");
    ok.textContent = opts.confirmLabel;
    if (opts.danger !== false) ok.classList.add("danger");
    cancel.onclick = () => {
      close();
      resolve(false);
    };
    ok.onclick = () => {
      close();
      resolve(true);
    };
    row.appendChild(cancel);
    row.appendChild(ok);
    root.appendChild(p);
    root.appendChild(row);
    ok.focus();
  });
}

// crates/herdr-desktop/ui/src/components/toasts.ts
function toast(kind, text, ms = 4e3) {
  let root = document.getElementById("toasts-root");
  if (!root) {
    root = document.createElement("div");
    root.id = "toasts-root";
    document.body.appendChild(root);
  }
  const el = document.createElement("div");
  el.className = `toast ${kind}`;
  el.textContent = text;
  root.appendChild(el);
  setTimeout(() => {
    el.classList.add("out");
    setTimeout(() => el.remove(), 250);
  }, ms);
}
function toastCopy(btn, doneLabel = "copied \u2713") {
  const orig = btn.textContent;
  btn.textContent = doneLabel;
  setTimeout(() => {
    btn.textContent = orig;
  }, 1200);
}

// crates/herdr-desktop/ui/src/components/topbar.ts
function fleetDot(cards) {
  if (!cards.length) return "gray";
  if (cards.some((c) => stateName(c.info.state) === "Errored")) return "red";
  if (cards.some((c) => stateName(c.info.state) === "Blocked")) return "amber";
  return "green";
}
async function copyDiagnostics(store2, invoke2) {
  const c = store2.summaryCounts();
  let ping = false;
  try {
    ping = await invoke2("ping_cmd");
  } catch {
    ping = false;
  }
  const diag = {
    app: "herdr-desktop",
    time: (/* @__PURE__ */ new Date()).toISOString(),
    daemon: ping ? "reachable" : "unreachable",
    connected: store2.state.connected,
    stale: store2.state.stale,
    agents: { total: c.total, working: c.working, blocked: c.blocked, errored: c.errored, idle: c.idle, exited: c.exited },
    hosts: store2.state.hosts.map(([h, up]) => ({ name: h.name, up }))
  };
  await navigator.clipboard.writeText(JSON.stringify(diag, null, 2));
  toast("ok", "Diagnostics copied");
}
function mountTopbar(store2, invoke2) {
  const connDot = req("conn-dot");
  const connLabel = req("conn-label");
  const fleetDotEl = req("fleet-dot");
  const summary = req("fleet-summary");
  req("btn-new-agent").onclick = () => document.getElementById("btn-spawn")?.click();
  req("btn-side").onclick = () => document.body.classList.toggle("no-side");
  mountMenu(store2, invoke2);
  store2.subscribe((s) => {
    connDot.classList.toggle("connected", s.connected);
    const pill = req("conn-pill");
    if (!s.connected) {
      connLabel.textContent = "Disconnected";
      pill.className = "pill down";
      connDot.title = "daemon: unreachable";
    } else if (s.stale) {
      connLabel.textContent = "Reconnecting\u2026";
      pill.className = "pill recon";
      connDot.title = "daemon: reconnecting";
    } else {
      connLabel.textContent = "Connected";
      pill.className = "pill up";
      connDot.title = "daemon: connected";
    }
    const c = store2.summaryCounts();
    summary.innerHTML = "";
    const chips = [
      [`${c.total}`, "total", "all"],
      [`\u25CF ${c.working}`, "Working", "Working"],
      [`\u25A0 ${c.blocked}`, "Blocked", "Blocked"],
      [`\u2716 ${c.errored}`, "Errored", "Errored"],
      [`\u25CC ${c.idle}`, "Idle", "Idle"],
      [`\xB7 ${c.exited}`, "Exited", "Exited"]
    ];
    for (const [text, cls, filter] of chips) {
      const b = document.createElement("button");
      b.className = `chip ${cls}${store2.state.filter === filter ? " on" : ""}`;
      b.textContent = text;
      b.title = `filter: ${filter}`;
      b.onclick = () => store2.setFilter(store2.state.filter === filter ? "all" : filter);
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
function mountMenu(store2, invoke2) {
  const btn = req("btn-menu");
  let open = null;
  const close = () => {
    open?.remove();
    open = null;
    document.removeEventListener("mousedown", outside, true);
  };
  const outside = (e) => {
    if (open && !e.target.closest("#menu-pop")) close();
  };
  btn.onclick = () => {
    if (open) {
      close();
      return;
    }
    const pop = document.createElement("div");
    pop.id = "menu-pop";
    pop.setAttribute("role", "menu");
    const item = (label, danger, run) => {
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
    item(`Kill all (${store2.state.cards.length})\u2026`, true, async () => {
      const n = store2.state.cards.length;
      if (!n) return;
      if (await confirmModal({
        title: "Kill all agents",
        body: `Kill all ${n} agent${n === 1 ? "" : "s"}? This cannot be undone.`,
        confirmLabel: `kill ${n} agent${n === 1 ? "" : "s"}`
      })) {
        await invoke2("kill_all_cmd");
        toast("ok", `Killed ${n} agent${n === 1 ? "" : "s"}`);
      }
    });
    item("Clear exited", false, () => {
      const n = store2.hideExited();
      toast("ok", n ? `Cleared ${n} exited` : "Nothing exited to clear");
    });
    item("Shutdown daemon\u2026", true, async () => {
      if (await confirmModal({
        title: "Shutdown daemon",
        body: "This stops the daemon and ALL agents (local and remote bridges). Clients will show disconnected. This cannot be undone.",
        confirmLabel: "shutdown daemon"
      })) {
        try {
          await invoke2("shutdown_cmd");
          toast("ok", "Daemon shutting down");
        } catch (e) {
          toast("err", `Shutdown failed: ${String(e)}`);
        }
      }
    });
    item("Copy diagnostics", false, () => void copyDiagnostics(store2, invoke2));
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

// crates/herdr-desktop/ui/src/components/spawn-modal.ts
var PRESETS = [
  { label: "Bash shell", profile: "bash", command: window.__HERDR_SHELL__ || "/bin/bash", args: "" },
  { label: "Generic command", profile: "generic", command: "", args: "" },
  { label: "Claude Code", profile: "claude-code", command: "claude", args: "" },
  { label: "Codex", profile: "codex", command: "codex", args: "" }
];
var RECENT_KEY = "herdr.spawn.recent";
var RECENT_KEPT = 5;
function loadRecent() {
  try {
    return JSON.parse(localStorage.getItem(RECENT_KEY) || "[]");
  } catch {
    return [];
  }
}
function saveRecent(r) {
  const all = [r, ...loadRecent().filter((x) => JSON.stringify(x) !== JSON.stringify(r))];
  try {
    localStorage.setItem(RECENT_KEY, JSON.stringify(all.slice(0, RECENT_KEPT)));
  } catch {
  }
}
function field(labelText, el) {
  const wrap = document.createElement("label");
  wrap.className = "form-field";
  const span = document.createElement("span");
  span.textContent = labelText;
  wrap.appendChild(span);
  wrap.appendChild(el);
  return wrap;
}
function openSpawnModal(store2, invoke2, prefill) {
  const { root, close } = openModalShell("Spawn agent");
  const profileSel = document.createElement("select");
  profileSel.setAttribute("aria-label", "profile");
  for (const p of ["generic", "claude-code", "codex", "bash"]) {
    const o = document.createElement("option");
    o.value = o.textContent = p;
    profileSel.appendChild(o);
  }
  if (prefill?.profile) profileSel.value = prefill.profile;
  const hostSel = document.createElement("select");
  hostSel.setAttribute("aria-label", "host");
  const localOpt = document.createElement("option");
  localOpt.value = "";
  localOpt.textContent = "local";
  hostSel.appendChild(localOpt);
  for (const [host] of store2.state.hosts) {
    const o = document.createElement("option");
    o.value = host.name;
    o.textContent = host.name;
    hostSel.appendChild(o);
  }
  hostSel.value = prefill?.host ?? store2.state.selectedHost ?? "";
  const cwdInput = document.createElement("input");
  cwdInput.value = prefill?.cwd ?? "/tmp";
  cwdInput.setAttribute("aria-label", "working directory");
  const cmdInput = document.createElement("input");
  cmdInput.placeholder = "/bin/bash";
  cmdInput.value = prefill?.command ?? "";
  cmdInput.setAttribute("aria-label", "command");
  const argsInput = document.createElement("input");
  argsInput.placeholder = "(space-separated)";
  argsInput.value = prefill?.args ?? "";
  argsInput.setAttribute("aria-label", "args");
  const presetRow = document.createElement("div");
  presetRow.className = "preset-row";
  for (const p of PRESETS) {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "ghost";
    b.textContent = p.label;
    b.onclick = () => {
      profileSel.value = p.profile;
      cmdInput.value = p.command;
      argsInput.value = p.args;
      updatePreview();
      cmdInput.focus();
    };
    presetRow.appendChild(b);
  }
  const recent = loadRecent();
  if (recent.length) {
    const rlabel = document.createElement("div");
    rlabel.className = "muted";
    rlabel.textContent = "recent:";
    presetRow.appendChild(rlabel);
    for (const r of recent) {
      const b = document.createElement("button");
      b.type = "button";
      b.className = "ghost";
      b.textContent = `${r.profile} \xB7 ${r.command || "(cmd)"}`;
      b.title = `${r.profile} on ${r.host || "local"} in ${r.cwd}: ${r.command} ${r.args}`;
      b.onclick = () => {
        profileSel.value = r.profile;
        hostSel.value = r.host || "";
        cwdInput.value = r.cwd;
        cmdInput.value = r.command;
        argsInput.value = r.args;
        updatePreview();
      };
      presetRow.appendChild(b);
    }
  }
  const preview = document.createElement("pre");
  preview.className = "command-preview terminal";
  const updatePreview = () => {
    const parts = ["herdr spawn", `--profile ${profileSel.value || "generic"}`];
    if (hostSel.value) parts.push(`--host ${hostSel.value}`);
    parts.push(`--cwd ${cwdInput.value || "/tmp"}`, "--", cmdInput.value || "(command)", argsInput.value);
    preview.textContent = parts.filter(Boolean).join(" ");
  };
  for (const el of [profileSel, hostSel, cwdInput, cmdInput, argsInput]) {
    el.addEventListener("input", updatePreview);
  }
  updatePreview();
  const err = document.createElement("div");
  err.className = "form-error";
  err.hidden = true;
  const actions = document.createElement("div");
  actions.className = "modal-actions";
  const cancel = document.createElement("button");
  cancel.textContent = "cancel";
  cancel.onclick = close;
  const spawn = document.createElement("button");
  spawn.textContent = "spawn";
  spawn.onclick = async () => {
    if (!cmdInput.value.trim()) {
      err.textContent = "Command is required (pick a preset or type one).";
      err.hidden = false;
      cmdInput.focus();
      return;
    }
    spawn.disabled = true;
    try {
      const rec = {
        profile: profileSel.value,
        host: hostSel.value || null,
        cwd: cwdInput.value || "/tmp",
        command: cmdInput.value.trim(),
        args: argsInput.value.trim()
      };
      await invoke2("spawn_agent_cmd", {
        ...rec,
        args: rec.args ? rec.args.split(/\s+/) : []
      });
      saveRecent(rec);
      store2.noteSpawnOptimistic(rec.profile, rec.host);
      toast("ok", `Spawning ${rec.profile}\u2026`);
      close();
    } catch (e) {
      err.textContent = `Spawn failed: ${String(e)}`;
      err.hidden = false;
      spawn.disabled = false;
    }
  };
  actions.appendChild(cancel);
  actions.appendChild(spawn);
  root.appendChild(presetRow);
  root.appendChild(field("profile:", profileSel));
  root.appendChild(field("host:", hostSel));
  root.appendChild(field("cwd:", cwdInput));
  root.appendChild(field("command:", cmdInput));
  root.appendChild(field("args:", argsInput));
  root.appendChild(preview);
  root.appendChild(err);
  root.appendChild(actions);
  cmdInput.focus();
}
function mountSpawnEntry(store2, invoke2) {
  req("btn-spawn").onclick = () => openSpawnModal(store2, invoke2);
}

// crates/herdr-desktop/ui/src/components/sidebar.ts
var FLEET_ROWS = [
  ["all", "All", "\u25CF"],
  ["attention", "Needs attention", "\u25A0"],
  ["Working", "Working", "\u25CF"],
  ["Blocked", "Blocked", "\u25A0"],
  ["Errored", "Errored", "\u2716"],
  ["Idle", "Idle", "\u25CC"],
  ["Exited", "Exited", "\xB7"]
];
var PROFILE_CHIPS = [
  ["bash", "Bash"],
  ["claude-code", "Claude Code"],
  ["codex", "Codex"],
  ["generic", "Generic"]
];
function mountSidebar(store2, invoke2) {
  const hostList = req("host-list");
  const fleetList = req("fleet-filters");
  const chips = req("profile-chips");
  for (const [profile, label] of PROFILE_CHIPS) {
    const b = document.createElement("button");
    b.className = "ghost chip-btn";
    b.textContent = label;
    b.title = `spawn ${label} agent\u2026`;
    b.onclick = () => openSpawnModal(store2, invoke2, { profile });
    chips.appendChild(b);
  }
  req("btn-add-remote").onclick = async () => {
    const name = window.prompt("Remote name (used with spawn --host):");
    if (!name) return;
    const sshTarget = window.prompt(`SSH target for "${name}" (host or user@host):`);
    if (!sshTarget) return;
    try {
      await invoke2("remote_add_cmd", { name, sshTarget, port: 22, user: null });
      toast("ok", `Remote ${name} added`);
    } catch (e) {
      toast("err", `Add remote failed: ${String(e)}`);
    }
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
    renderFleet(s.filter);
    renderHosts();
  });
  hostList.onclick = async (e) => {
    const li = e.target?.closest("li[data-host]");
    if (!li) return;
    if (e.target?.closest("button.rm-host")) {
      const name = li.dataset["host"];
      const live = store2.state.cards.filter((c) => (c.info.host || null) === name).length;
      if (await confirmModal({
        title: "Forget host",
        body: live ? `Forget host ${name}? Its ${live} live agent${live === 1 ? "" : "s"} will be disconnected.` : `Forget host ${name}?`,
        confirmLabel: "forget host"
      })) {
        try {
          await invoke2("remote_remove_cmd", { name });
          toast("ok", `Host ${name} forgotten`);
        } catch (err) {
          toast("err", `Remove failed: ${String(err)}`);
        }
        if (store2.state.selectedHost === name) store2.state.selectedHost = null;
        refreshHosts();
      }
      return;
    }
    store2.state.selectedHost = li.dataset["host"] === "" ? null : li.dataset["host"];
    renderHosts();
  };
  fleetList.onclick = (e) => {
    const li = e.target?.closest("li[data-filter]");
    if (!li) return;
    store2.setFilter(li.dataset["filter"]);
  };
  function renderFleet(active) {
    const c = store2.summaryCounts();
    const counts = {
      all: c.total,
      attention: c.blocked + c.errored,
      Working: c.working,
      Blocked: c.blocked,
      Errored: c.errored,
      Idle: c.idle,
      Exited: c.exited
    };
    fleetList.innerHTML = "";
    for (const [key, label, glyph] of FLEET_ROWS) {
      const li = document.createElement("li");
      if (active === key) li.classList.add("selected");
      li.dataset["filter"] = key;
      li.innerHTML = `<span class="glyph">${glyph}</span><span class="host-name">${label}</span><span class="count">${counts[key] ?? 0}</span>`;
      fleetList.appendChild(li);
    }
  }
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
      li.dataset["host"] = key;
      if ((store2.state.selectedHost || "") === key) li.classList.add("selected");
      li.innerHTML = `<span class="host-name">${label}</span><span class="count">${count}</span>`;
      if (key !== "") {
        const rm = document.createElement("button");
        rm.className = "rm-host ghost";
        rm.title = `forget host ${key}`;
        rm.setAttribute("aria-label", `forget host ${key}`);
        rm.textContent = "\xD7";
        li.appendChild(rm);
      }
      hostList.appendChild(li);
    }
  }
}

// crates/herdr-desktop/ui/src/components/agent-list.ts
function runtimeOf(card) {
  return relTime(card.info.started_at_unix_ms);
}
function relTime(ms) {
  const s = Math.max(0, Math.floor((Date.now() - ms) / 1e3));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86400)}d`;
}
function sparkBars(bars) {
  if (!bars.length) return "";
  const max = Math.max(...bars, 1);
  return bars.map((v) => `<span style="height:${Math.max(2, Math.round(v / max * 14))}px"></span>`).join("");
}
function mountAgentList(store2, invoke2, onSelect) {
  const list = req("agent-list");
  const search = req("fleet-search");
  const sort = req("fleet-sort");
  search.addEventListener("input", () => store2.setSearch(search.value));
  sort.onchange = () => store2.setSort(sort.value);
  store2.subscribe((s) => renderList(store2.visibleAgents(), s.selectedId));
  function renderList(visible, selectedId) {
    list.innerHTML = "";
    renderPending();
    if (!visible.length && !store2.state.pendingSpawns.length) {
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
      li.textContent = `Showing ${CAP} of ${visible.length} \u2014 refine search or filters.`;
      list.appendChild(li);
    }
  }
  function renderPending() {
    for (const p of store2.state.pendingSpawns) {
      const li = document.createElement("li");
      li.className = "agent-row pending";
      li.innerHTML = `<span class="id muted">spawning\u2026</span><span class="profile">${esc(p.profile)}</span><span class="badge Starting">\u2026</span><span class="last-line muted">waiting for AgentSpawned</span>`;
      list.appendChild(li);
    }
  }
  function renderRow(card, selectedId) {
    const li = document.createElement("li");
    li.className = "agent-row";
    if (card.info.id === selectedId) li.classList.add("selected");
    const st = stateName(card.info.state);
    if (st === "Blocked" || st === "Errored") li.classList.add("needs-attention");
    const bars = sparkBars(store2.state.activity[card.info.id] || []);
    const host = card.info.host || "local";
    li.innerHTML = `<span class="id">${esc(card.info.id.slice(0, 12))}</span><span class="profile">${esc(card.info.profile)}</span><span class="badge ${esc(st)}">${esc(st)}</span><span class="last-line">${esc(lastLine(card.log_tail || "")) || "&nbsp;"}</span><span class="meta muted">${esc(host)} \xB7 ${esc(card.info.cwd)} \xB7 ${runtimeOf(card)} \xB7 active ${relTime(card.info.last_output_unix_ms)} ago</span><span class="spark" aria-hidden="true">${bars}</span>`;
    li.onclick = () => onSelect(card.info.id);
    const copy = document.createElement("button");
    copy.className = "row-copy ghost";
    copy.title = `copy id ${card.info.id}`;
    copy.setAttribute("aria-label", `copy id ${card.info.id}`);
    copy.textContent = "\u29C9";
    copy.onclick = async (e) => {
      e.stopPropagation();
      await navigator.clipboard.writeText(card.info.id);
      copy.textContent = "\u2713";
      setTimeout(() => {
        copy.textContent = "\u29C9";
      }, 1200);
    };
    const kill = document.createElement("button");
    kill.className = "row-kill ghost";
    kill.title = `kill ${card.info.id}`;
    kill.setAttribute("aria-label", `kill ${card.info.id}`);
    kill.textContent = "\xD7";
    kill.onclick = async (e) => {
      e.stopPropagation();
      if (await confirmModal({
        title: "Kill agent",
        body: `Kill agent ${card.info.id} (${card.info.profile}, last line: ${lastLine(card.log_tail || "").slice(0, 80)})? This can't be undone.`,
        confirmLabel: "kill agent"
      })) {
        await invoke2("kill_agent_cmd", { agentId: card.info.id });
      }
    };
    li.appendChild(copy);
    li.appendChild(kill);
    return li;
  }
  function renderEmpty() {
    const s = store2.state;
    const li = document.createElement("li");
    li.className = "empty-state";
    if (!s.connected) {
      li.innerHTML = `<h3>Daemon unavailable</h3><p>The desktop cannot reach the daemon socket. Start it, then retry \u2014 this view resyncs automatically.</p>`;
      const retry = document.createElement("button");
      retry.textContent = "retry now";
      retry.onclick = () => invoke2("snapshot_cmd").then((snap) => store2.applySnapshot(snap));
      li.appendChild(retry);
    } else if (!s.cards.length) {
      li.innerHTML = `<h3>No agents running</h3><p>Spawn a shell, Claude Code, Codex, or any CLI process \u2014 or paste one of these in a terminal:</p><pre class="terminal">herdr spawn --profile bash --cwd /tmp -- bash --norc -i
herdr spawn --profile claude-code --cwd ~/project -- claude</pre>`;
      const spawn = document.createElement("button");
      spawn.textContent = "spawn agent";
      spawn.onclick = () => openSpawnModal(store2, invoke2);
      li.appendChild(spawn);
      const presets = document.createElement("div");
      presets.className = "preset-row";
      for (const [profile, label] of [
        ["bash", "Bash"],
        ["claude-code", "Claude Code"],
        ["codex", "Codex"],
        ["generic", "Generic"]
      ]) {
        const b = document.createElement("button");
        b.className = "ghost";
        b.textContent = label;
        b.onclick = () => openSpawnModal(store2, invoke2, { profile });
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
      li.innerHTML = `<h3>No agents match</h3><p>Adjust search, state filter, or host scope.</p>`;
    }
    list.appendChild(li);
  }
  document.addEventListener("keydown", (e) => {
    if (/INPUT|SELECT|TEXTAREA/.test(document.activeElement?.tagName ?? "")) return;
    const visible = store2.visibleAgents();
    if (!visible.length) return;
    if (e.key === "/") {
      e.preventDefault();
      search.focus();
      return;
    }
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
  const mediaTabBtn = req("tabbtn-media");
  const emptyPane = req("detail-empty");
  const tabsRow = document.querySelector("#detail-header .tabs");
  req("btn-empty-spawn").onclick = () => document.getElementById("btn-spawn")?.click();
  const headerActions = [req("detail-copy"), req("detail-kill")];
  const exitedBar = req("exited-bar");
  const exitedText = req("exited-text");
  req("btn-copy-exit-logs").onclick = async (e) => {
    const card = store2.state.cards.find((c) => c.info.id === store2.state.selectedId);
    if (card) {
      await navigator.clipboard.writeText(card.log_tail || "");
      toastCopy(e.target);
    }
  };
  req("btn-respawn-like").onclick = () => {
    const card = store2.state.cards.find((c) => c.info.id === store2.state.selectedId);
    if (card) {
      openSpawnModal(store2, invoke2, {
        profile: card.info.profile,
        host: card.info.host,
        cwd: card.info.cwd,
        command: card.info.command,
        args: ""
      });
    }
  };
  req("btn-kill-exited").onclick = async () => {
    const id = store2.state.selectedId;
    if (!id) return;
    if (await confirmModal({
      title: "Kill agent",
      body: `Kill agent ${id}? The PTY is destroyed; ring-buffer history stays until resync drops it.`,
      confirmLabel: "kill agent"
    })) {
      await invoke2("kill_agent_cmd", { agentId: id });
    }
  };
  const panels = {
    thread: req("tab-chat"),
    terminal: req("tab-terminal"),
    events: req("tab-events"),
    media: req("tab-media"),
    info: req("tab-info")
  };
  tabs.forEach((btn) => {
    btn.onclick = () => {
      const id = store2.state.selectedId;
      if (id) store2.setTab(id, btn.dataset["tab"]);
      else showTab("thread");
    };
  });
  req("detail-kill").onclick = async () => {
    const id = store2.state.selectedId;
    if (!id) return;
    if (await confirmModal({
      title: "Kill agent",
      body: `Kill agent ${id}? The PTY is destroyed; ring-buffer history stays until resync drops it.`,
      confirmLabel: "kill agent"
    })) {
      await invoke2("kill_agent_cmd", { agentId: id });
    }
  };
  req("detail-copy").onclick = () => {
    const id = store2.state.selectedId;
    if (id) navigator.clipboard.writeText(id);
  };
  function showTab(tab) {
    emptyPane.hidden = true;
    tabs.forEach((b) => {
      const active = b.dataset["tab"] === tab;
      b.classList.toggle("active", active);
      b.setAttribute("aria-selected", String(active));
    });
    for (const [name, el] of Object.entries(panels)) {
      el.hidden = name !== tab;
    }
  }
  function showEmpty() {
    for (const el of Object.values(panels)) el.hidden = true;
    tabs.forEach((b) => {
      b.classList.toggle("active", false);
      b.setAttribute("aria-selected", "false");
    });
    if (tabsRow) tabsRow.hidden = true;
    for (const b of headerActions) b.hidden = true;
    exitedBar.hidden = true;
    emptyPane.hidden = false;
  }
  store2.subscribe((s) => {
    const card = s.cards.find((c) => c.info.id === s.selectedId);
    if (!card) {
      title.textContent = "no agent selected";
      showEmpty();
      return;
    }
    for (const b of headerActions) b.hidden = false;
    if (tabsRow) tabsRow.hidden = false;
    emptyPane.hidden = true;
    const st = stateName(card.info.state);
    const dead = st === "Exited" || st === "Errored";
    exitedBar.hidden = !dead;
    if (dead) exitedText.textContent = `Agent ${st === "Exited" ? "exited" : "errored"} \u2014 sending is disabled.`;
    mediaTabBtn.hidden = !(card.media && card.media.length);
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
    if (!card || store2.tabFor(card.info.id, card.info.profile) !== "thread") return;
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
      div.innerHTML = `<span class="who">${esc(who)}</span><pre>${esc(stripAnsi(t.text))}</pre>`;
      turnsEl.appendChild(div);
    }
    panel.scrollTop = panel.scrollHeight;
  }
}

// crates/herdr-desktop/ui/src/components/detail-terminal.ts
function mountDetailTerminal(store2, invoke2) {
  const panel = req("tab-terminal");
  const logEl = req("detail-log");
  const followBtn = req("btn-follow");
  const pill = req("paused-pill");
  const meta = req("terminal-meta");
  let frozenId = null;
  let frozenLen = 0;
  let unseen = 0;
  followBtn.onclick = () => store2.setFollow(!store2.state.follow);
  pill.onclick = () => store2.setFollow(true);
  req("btn-copy-logs").onclick = async (e) => {
    const card = store2.state.cards.find((c) => c.info.id === store2.state.selectedId);
    if (card) {
      await navigator.clipboard.writeText(card.log_tail || "");
      toastCopy(e.target, "copied \u2713");
    }
  };
  req("btn-load-full").onclick = async () => {
    const id = store2.state.selectedId;
    if (!id) return;
    try {
      const payload = await invoke2("logs_cmd", { agentId: id, bytes: 262144 });
      store2.setLogTail(id, payload);
      toast("ok", "Full buffer loaded");
    } catch (e) {
      toast("err", `Load failed: ${String(e)}`);
    }
  };
  store2.subscribe((s) => {
    followBtn.textContent = `follow: ${s.follow ? "on" : "off"}`;
    const card = s.cards.find((c) => c.info.id === s.selectedId);
    if (!card || store2.tabFor(card.info.id, card.info.profile) !== "terminal") return;
    const tail = card.log_tail || "";
    if (s.follow || frozenId !== card.info.id) {
      logEl.innerHTML = ansiToHtml(tail);
      frozenId = card.info.id;
      frozenLen = tail.length;
      unseen = 0;
      pill.hidden = true;
      panel.scrollTop = panel.scrollHeight;
    } else {
      const fresh = tail.slice(frozenLen);
      unseen += fresh.split("\n").length - 1;
      frozenLen = tail.length;
      if (unseen > 0) {
        pill.hidden = false;
        pill.textContent = `Paused \xB7 ${unseen} new \xB7 Resume`;
      }
    }
    meta.textContent = `${tail.length.toLocaleString()} chars shown \xB7 ring buffer 256 KB`;
  });
}

// crates/herdr-desktop/ui/src/components/detail-events.ts
function mountDetailEvents(store2) {
  const list = req("events-timeline");
  store2.subscribe((s) => {
    const card = s.cards.find((c) => c.info.id === s.selectedId);
    if (!card || store2.tabFor(card.info.id, card.info.profile) !== "events") return;
    renderTimeline(s.timeline[card.info.id] || []);
  });
  function renderTimeline(rows) {
    list.innerHTML = "";
    if (!rows.length) {
      const li = document.createElement("li");
      li.className = "muted";
      li.textContent = "No events yet for this agent.";
      list.appendChild(li);
      return;
    }
    for (const r of [...rows].reverse()) {
      const li = document.createElement("li");
      li.className = `ev ev-${r.kind}`;
      const time = document.createElement("span");
      time.className = "ev-time muted";
      time.textContent = new Date(r.t).toLocaleTimeString();
      const kind = document.createElement("span");
      kind.className = "ev-kind";
      kind.textContent = r.kind;
      const text = document.createElement("span");
      text.className = "ev-text";
      text.textContent = r.text;
      li.appendChild(time);
      li.appendChild(kind);
      li.appendChild(text);
      list.appendChild(li);
    }
  }
}

// crates/herdr-desktop/ui/src/components/detail-media.ts
function mountDetailMedia(store2) {
  const gallery = req("media-gallery");
  store2.subscribe((s) => {
    const card = s.cards.find((c) => c.info.id === s.selectedId);
    if (!card || store2.tabFor(card.info.id, card.info.profile) !== "media") return;
    gallery.innerHTML = "";
    const media = card.media || [];
    if (!media.length) {
      gallery.innerHTML = `<div class="muted">No media from this agent yet.</div>`;
      return;
    }
    media.forEach((m, i) => {
      const fig = document.createElement("figure");
      const url = `data:${m.mime};base64,${m.data_base64}`;
      if (m.mime.startsWith("image/")) {
        const img = document.createElement("img");
        img.src = url;
        img.alt = m.caption || `media ${i + 1}`;
        fig.appendChild(img);
      } else {
        const div = document.createElement("div");
        div.className = "media-blob muted";
        div.textContent = m.mime;
        fig.appendChild(div);
      }
      const cap = document.createElement("figcaption");
      cap.textContent = `${m.caption || `media ${i + 1}`} \xB7 ${m.mime}`;
      fig.appendChild(cap);
      const copy = document.createElement("button");
      copy.className = "ghost";
      copy.textContent = "copy data URL";
      copy.onclick = () => navigator.clipboard.writeText(url);
      fig.appendChild(copy);
      gallery.appendChild(fig);
    });
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
  const raw = req("composer-raw");
  const bar = req("composer-bar");
  const warn = req("composer-warn");
  const sendBtn = req("composer-send");
  req("composer-send").onclick = sendLine;
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      sendLine();
    }
  });
  async function sendLine() {
    const id = store2.state.selectedId;
    if (!id || !input.value) return;
    const text = input.value;
    const useRaw = raw.checked;
    input.value = "";
    warn.hidden = true;
    try {
      await invoke2("send_input_cmd", { agentId: id, text, raw: useRaw });
      store2.noteSentLocal(id, text);
    } catch (e) {
      warn.textContent = `Send failed: ${String(e)}`;
      warn.hidden = false;
    }
  }
  store2.subscribe((s) => {
    const card = s.cards.find((c) => c.info.id === s.selectedId);
    const st = card ? stateName(card.info.state) : null;
    const dead = !card || st === "Exited" || st === "Errored";
    sendBtn.disabled = dead;
    input.disabled = dead;
    bar.classList.toggle("blocked", st === "Blocked");
    if (!card) {
      warn.textContent = "Select an agent to send input.";
      warn.hidden = false;
    } else if (dead) {
      warn.textContent = `Agent ${st === "Exited" ? "exited" : "errored"} \u2014 sending is disabled.`;
      warn.hidden = false;
    } else warn.hidden = true;
  });
}

// crates/herdr-desktop/ui/src/components/conn-banner.ts
function mountConnBanner(store2, invoke2) {
  const banner = req("conn-banner");
  store2.subscribe((s) => {
    banner.hidden = s.connected;
    if (s.connected) return;
    banner.innerHTML = "";
    const msg = document.createElement("span");
    const retry = document.createElement("button");
    retry.className = "ghost";
    retry.textContent = "retry now";
    retry.onclick = () => invoke2("snapshot_cmd").then((snap) => {
      store2.applySnapshot(snap);
      store2.setConnected(true);
    }).catch(() => {
    });
    const diag = document.createElement("button");
    diag.className = "ghost";
    diag.textContent = "copy diagnostics";
    diag.onclick = async () => {
      const c = store2.summaryCounts();
      await navigator.clipboard.writeText(
        JSON.stringify(
          {
            time: (/* @__PURE__ */ new Date()).toISOString(),
            connected: s.connected,
            stale: s.stale,
            agents: c
          },
          null,
          2
        )
      );
      retry.textContent = "copied \u2713";
      setTimeout(() => {
        retry.textContent = "retry now";
      }, 1200);
    };
    if (s.stale) {
      banner.classList.toggle("reconnecting", true);
      msg.textContent = "Reconnecting to daemon \u2014 showing last known fleet (stale). ";
    } else {
      banner.classList.toggle("reconnecting", false);
      msg.textContent = "Daemon unreachable \u2014 start it with `herdr daemon`, then retry. ";
    }
    banner.appendChild(msg);
    banner.appendChild(retry);
    banner.appendChild(diag);
  });
}

// crates/herdr-desktop/ui/src/components/palette.ts
function mountPalette(store2, invoke2) {
  const root = req("palette-root");
  req("btn-palette").onclick = open;
  function open() {
    root.innerHTML = "";
    root.hidden = false;
    const box = document.createElement("div");
    box.className = "palette";
    box.setAttribute("role", "dialog");
    box.setAttribute("aria-label", "command palette");
    const input = document.createElement("input");
    input.placeholder = "type a command or agent id\u2026";
    input.setAttribute("aria-label", "command palette");
    const list = document.createElement("ul");
    box.appendChild(input);
    box.appendChild(list);
    root.appendChild(box);
    const prevFocus = document.activeElement;
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
    const onKey = (e) => {
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
      filtered = q ? actions.filter((a) => `${a.label} ${a.hint}`.toLowerCase().includes(q)) : actions;
      idx = 0;
      render();
    });
    root.onmousedown = (e) => {
      if (e.target === root) close();
    };
    render();
    input.focus();
  }
  function buildActions() {
    const s = store2.state;
    const sel = s.cards.find((c) => c.info.id === s.selectedId);
    const out = [
      { id: "spawn", label: "Spawn agent\u2026", hint: "profile \xB7 host \xB7 cwd", run: () => openSpawnModal(store2, invoke2) }
    ];
    for (const [profile, label] of [
      ["bash", "Spawn Bash shell"],
      ["claude-code", "Spawn Claude Code"],
      ["codex", "Spawn Codex"],
      ["generic", "Spawn generic command"]
    ]) {
      out.push({
        id: `preset-${profile}`,
        label,
        hint: "spawn preset",
        run: () => openSpawnModal(store2, invoke2, { profile })
      });
    }
    for (const c of s.cards) {
      const id = c.info.id;
      out.push({
        id: `go-${id}`,
        label: `Go to ${id.slice(0, 12)}`,
        hint: `${c.info.profile} \xB7 ${stateName(c.info.state)}`,
        run: () => store2.select(id)
      });
    }
    for (const [host] of s.hosts) {
      out.push({
        id: `host-${host.name}`,
        label: `Switch host \u2192 ${host.name}`,
        hint: "sidebar filter",
        run: () => {
          store2.state.selectedHost = host.name;
        }
      });
    }
    out.push({
      id: "host-local",
      label: "Switch host \u2192 local",
      hint: "sidebar filter",
      run: () => {
        store2.state.selectedHost = null;
      }
    });
    if (sel) {
      const id = sel.info.id;
      out.push(
        {
          id: "copy",
          label: `Copy id ${id.slice(0, 12)}`,
          hint: "clipboard",
          run: () => navigator.clipboard.writeText(id)
        },
        {
          id: "follow",
          label: `${store2.state.follow ? "Pause" : "Resume"} follow`,
          hint: "terminal tail",
          run: () => store2.setFollow(!store2.state.follow)
        },
        {
          id: "kill",
          label: `Kill ${id.slice(0, 12)}\u2026`,
          hint: "confirm",
          run: async () => {
            if (await confirmModal({
              title: "Kill agent",
              body: `Kill agent ${id}? The PTY is destroyed; ring-buffer history stays until resync drops it.`,
              confirmLabel: "kill agent"
            })) {
              await invoke2("kill_agent_cmd", { agentId: id });
            }
          }
        }
      );
      for (const tab of ["thread", "terminal", "events", "info"]) {
        out.push({
          id: `tab-${tab}`,
          label: `Switch tab \u2192 ${tab}`,
          hint: "detail pane",
          run: () => store2.setTab(id, tab)
        });
      }
    } else {
      out.push({
        id: "kill-none",
        label: "Kill selected",
        hint: "no agent selected",
        run: () => toast("info", "No agent selected")
      });
    }
    if (s.cards.length) {
      out.push({
        id: "kill-all",
        label: `Kill all ${s.cards.length} agents\u2026`,
        hint: "confirm",
        run: async () => {
          if (await confirmModal({
            title: "Kill all agents",
            body: `Kill all ${s.cards.length} agents? This cannot be undone.`,
            confirmLabel: `kill ${s.cards.length} agents`
          })) {
            await invoke2("kill_all_cmd");
          }
        }
      });
    }
    out.push(
      {
        id: "reconnect",
        label: "Reconnect now",
        hint: "resync snapshot",
        run: async () => {
          try {
            const snap = await invoke2("snapshot_cmd");
            store2.applySnapshot(snap);
            store2.setConnected(true);
            toast("ok", "Reconnected");
          } catch {
            toast("err", "Still unreachable \u2014 is the daemon running?");
          }
        }
      },
      {
        id: "shutdown",
        label: "Shutdown daemon\u2026",
        hint: "confirm",
        run: async () => {
          if (await confirmModal({
            title: "Shutdown daemon",
            body: "This stops the daemon and ALL agents. This cannot be undone.",
            confirmLabel: "shutdown daemon"
          })) {
            try {
              await invoke2("shutdown_cmd");
              toast("ok", "Daemon shutting down");
            } catch (e) {
              toast("err", `Shutdown failed: ${String(e)}`);
            }
          }
        }
      },
      {
        id: "diag",
        label: "Copy diagnostics",
        hint: "clipboard",
        run: async () => {
          const c = store2.summaryCounts();
          await navigator.clipboard.writeText(
            JSON.stringify({ time: (/* @__PURE__ */ new Date()).toISOString(), agents: c }, null, 2)
          );
          toast("ok", "Diagnostics copied");
        }
      }
    );
    return out;
  }
  document.addEventListener("keydown", (e) => {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
      e.preventDefault();
      if (root.hidden) open();
    }
  });
}

// crates/herdr-desktop/ui/src/main.ts
function fatal(msg) {
  const el = req("fatal");
  el.hidden = false;
  el.textContent = msg;
}
if (!window.__TAURI__) {
  fatal(
    "You opened the Herdr UI outside its host \u2014 this looks like a plain browser tab, where it cannot reach the daemon. Run the desktop app instead: 1) start the daemon with `herdr daemon`, 2) `cargo run -p herdr-desktop --features tauri`. Or open the static preview: `python3 scripts/make_preview.py`, then open `target/preview/index.html`."
  );
  throw new Error("missing window.__TAURI__");
}
var { invoke } = window.__TAURI__.core;
var { listen } = window.__TAURI__.event;
var store = createStore();
mountTopbar(store, invoke);
mountTray(store);
mountSidebar(store, invoke);
mountSpawnEntry(store, invoke);
mountAgentList(store, invoke, (id) => store.select(id));
mountDetail(store, invoke);
mountDetailChat(store);
mountDetailTerminal(store, invoke);
mountDetailEvents(store);
mountDetailMedia(store);
mountDetailInfo(store);
mountComposer(store, invoke);
mountConnBanner(store, invoke);
mountPalette(store, invoke);
await listen("herdr://event", ({ payload }) => {
  const ev = payload && (payload.event || payload);
  for (const notice of store.applyEvent(ev)) {
    const m = notice.match(/^spawned:([0-9a-f]+):(\S+)$/);
    if (m) {
      toast("ok", `Spawned ${m[1].slice(0, 12)} (${m[2]})`);
      req("composer").focus();
    }
  }
});
await listen("herdr://conn", ({ payload }) => {
  if (payload === "connected") {
    const wasDown = !store.state.connected || store.state.stale;
    store.setConnected(true);
    invoke("snapshot_cmd").then((snap) => {
      store.applySnapshot(snap);
      if (wasDown) {
        const n = store.state.cards.length;
        toast("ok", `Reconnected \u2014 resynced ${n} agent${n === 1 ? "" : "s"}`);
      }
    });
  } else {
    store.setConnected(false);
  }
});
await listen("herdr://open-spawn", () => openSpawnModal(store, invoke));
invoke("snapshot_cmd").then((snap) => {
  store.applySnapshot(snap);
  store.setConnected(true);
}).catch(() => {
  store.setConnected(false);
});
document.addEventListener("keydown", (e) => {
  const inField = /INPUT|SELECT|TEXTAREA/.test(document.activeElement?.tagName ?? "");
  if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "n") {
    e.preventDefault();
    openSpawnModal(store, invoke);
    return;
  }
  if ((e.metaKey || e.ctrlKey) && e.shiftKey && e.key.toLowerCase() === "k") {
    e.preventDefault();
    const id2 = store.state.selectedId;
    if (id2) {
      confirmModal({
        title: "Kill agent",
        body: `Kill agent ${id2}? The PTY is destroyed; ring-buffer history stays until resync drops it.`,
        confirmLabel: "kill agent"
      }).then((ok) => {
        if (ok) invoke("kill_agent_cmd", { agentId: id2 });
      });
    }
    return;
  }
  if (inField) return;
  const id = store.state.selectedId;
  if (e.key >= "1" && e.key <= "5" && id) {
    const tabs = ["thread", "terminal", "events", "media", "info"];
    store.setTab(id, tabs[Number(e.key) - 1]);
  } else if (e.key === "f") {
    store.setFollow(!store.state.follow);
  } else if ((e.key === "g" || e.key === "G") && id) {
    store.setTab(id, "terminal");
    store.setFollow(e.key === "G");
    requestAnimationFrame(() => {
      const panel = document.getElementById("tab-terminal");
      if (panel) panel.scrollTop = e.key === "G" ? panel.scrollHeight : 0;
    });
  }
});
setInterval(() => store.touch(), 3e4);
window.__HERDR_APP__ = {
  get cards() {
    return store.state.cards;
  },
  get store() {
    return store;
  },
  applySnapshot: (snap) => store.applySnapshot(snap)
};
