/* herdr preview harness — simulates the Tauri IPC surface so the real
   dashboard UI code can run in a plain browser page.

   The dashboard's app.js is inlined below with two adaptations:
   1. window.__TAURI__ is provided by this shim instead of the Tauri runtime.
   2. The simulator emits the same `herdr://event` payloads the Rust shell
      forwards from the daemon's event bus, so every UI code path is the
      production one (spawn/output/state/media/exit/removal).

   The real app (no shim) is `cargo run -p herdr-desktop --features tauri`.
*/
(() => {
  // ---------- tiny event emitter used by the shim --------------------------
  const listeners = { event: [], conn: [] };
  function emit(channel, payload) {
    // no listeners yet — events before registration are dropped, matching Tauri.
    const key = channel.replace("herdr://", "");
    for (const fn of listeners[key] || []) fn({ payload });
  }

  // ---------- __TAURI__ shim ----------------------------------------------
  let snapCards = [];
  let snapChat = {};
  // Registered remotes, mirroring RemoteList's [[RemoteHost, bool]] shape.
  let registryHosts = [{ name: "dev", ssh_target: "dev.example.com", port: 22, user: null }];
  const invokeHandlers = {
    spawn_agent_cmd: (args) => simSpawn(args.profile || "generic", args.host || null),
    kill_all_cmd: () => {
      for (const c of [...snapCards]) simKill(c.info.id);
    },
    kill_agent_cmd: (args) => simKill(args.agentId),
    send_input_cmd: (args) => simOutput(args.agentId, `$ ${args.text}\n`),
    list_cmd: async () => snapCards.map((c) => ({ ...c.info })),
    logs_cmd: async (args) => ({ payload: LOG_TAILS[args.agentId] || "" }),
    ping_cmd: async () => true,
    snapshot_cmd: async () => ({
      cards: snapCards.map((c) => ({ ...c })),
      chat: Object.fromEntries(Object.entries(snapChat).map(([k, v]) => [k, v.map((t) => ({ ...t })) ])),
    }),
    remote_list_cmd: async () => registryHosts.map((h) => [{ ...h }, true]),
    remote_add_cmd: (args) => {
      registryHosts.push({ name: args.name, ssh_target: args.sshTarget, port: args.port || 22, user: args.user || null });
    },
    remote_remove_cmd: (args) => {
      registryHosts = registryHosts.filter((h) => h.name !== args.name);
    },
  };
  window.__TAURI__ = {
    core: {
      invoke: async (cmd, args) => {
        const h = invokeHandlers[cmd];
        return h ? h(args || {}) : Promise.reject(new Error(`unknown cmd ${cmd}`));
      },
    },
    event: {
      listen: (ev, fn) => {
        // Surface handler errors on window instead of losing them in the console.
        const wrapped = (msg) => {
          try {
            fn(msg);
          } catch (e) {
            (window.__HERDR_ERRS__ = window.__HERDR_ERRS__ || []).push(String((e && e.stack) || e));
          }
        };
        if (ev === "herdr://event") {
          listeners.event.push(wrapped);
        } else if (ev === "herdr://conn") {
          listeners.conn.push(wrapped);
          // Deterministic start: the moment the UI is listening, arm the seed.
          arm();
        }
        return Promise.resolve(() => {});
      },
    },
  };

  // ---------- fleet simulator ----------------------------------------------
  const LOG_TAILS = {};
  const id6 = () => Math.random().toString(16).slice(2, 8);
  function now() { return Date.now(); }

  function pushEvent(kind, body) {
    emit("herdr://event", { [kind]: body });
  }
  function info(id, profile, command, state, host) {
    return { id, profile, command, cwd: "/tmp", state, started_at_unix_ms: now(), last_output_unix_ms: now(), host: host || null, parent: null };
  }
  function tail(id, add) {
    LOG_TAILS[id] = ((LOG_TAILS[id] || "") + add).slice(-64 * 1024);
    return LOG_TAILS[id];
  }
  /** Preview-side chat synthesis (the real app gets segments from Rust). */
  function chatAppend(id, kind, text) {
    const turns = (snapChat[id] = snapChat[id] || []);
    const last = turns[turns.length - 1];
    if (last && last.kind === kind) last.text += "\n" + text;
    else turns.push({ kind, text });
  }
  function card(id, i) {
    const c = snapCards.find((x) => x.info.id === id);
    if (c) return c;
    const fresh = { info: i, log_tail: LOG_TAILS[id] || "", media: [] };
    snapCards.push(fresh);
    if (!snapChat[id]) snapChat[id] = [];
    return fresh;
  }

  function simSpawn(profile, host) {
    const id = id6() + id6();
    const i = info(id, profile, "/bin/bash", "Working", host);
    pushEvent("AgentSpawned", { info: i });
    card(id, i);
    setTimeout(() => simOutput(id, "\x1b[32m● agent booted\x1b[0m\n"), 250);
    return id;
  }

  function simOutput(id, payload) {
    pushEvent("AgentOutput", { agent_id: id, payload });
    const c = snapCards.find((x) => x.info.id === id);
    if (c) c.log_tail = tail(id, payload);
    chatAppend(id, "Agent", payload.replace(/\n$/, ""));
  }

  function simState(id, state) {
    const c = snapCards.find((x) => x.info.id === id);
    if (c) c.info.state = state;
    pushEvent("StateChange", { agent_id: id, state });
  }

  function simMedia(id) {
    pushEvent("AgentMedia", { agent_id: id, mime: "image/png", data_base64: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==", caption: "chart.png" });
    const c = snapCards.find((x) => x.info.id === id);
    if (c) { c.log_tail = tail(id, "[media: image/png]\n"); c.media = c.media || []; c.media.push({ mime: "image/png", data_base64: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==", caption: "chart.png" }); }
    chatAppend(id, "Tool", "[media: image/png]");
  }

  function simKill(id) {
    pushEvent("AgentExited", { agent_id: id, code: 137 });
    const c = snapCards.find((x) => x.info.id === id);
    if (c) c.info.state = "Errored";
    setTimeout(() => {
      pushEvent("AgentRemoved", { agent_id: id });
      snapCards = snapCards.filter((x) => x.info.id !== id);
      delete snapChat[id];
    }, 600);
  }

  // ---------- scripted boot -------------------------------------------------
  let armed = false;
  let booted = false;
  function arm() {
    armed = true;
    setTimeout(() => boot(), 0);
  }
  function boot() {
    if (!armed || booted) return;
    booted = true;
    // Connection comes up.
    setTimeout(() => {
      // Seed the fleet first, THEN announce the connection — the app pulls a
      // snapshot on "connected", and that snapshot must already contain the
      // fleet or it would wipe the cards the spawn events just created.
      const a = simSpawn("claude-code");
      const b = simSpawn("codex");
      const c = simSpawn("bash");
      const d = simSpawn("bash", "dev");
      chatAppend(a, "Human", "refactor the auth module");
      chatAppend(a, "Agent", "Planning refactor across 3 crates…");
      emit("herdr://conn", "connected");
      setTimeout(() => simOutput(a, "Planning refactor across 3 crates…\n"), 500);
      setTimeout(() => simState(a, "Working"), 600);
      setTimeout(() => simOutput(b, "running tests… 41 passed\n"), 800);
      setTimeout(() => simState(b, "Idle"), 900);
      setTimeout(() => simOutput(c, "$ tail -f deploy.log\n"), 1100);
      setTimeout(() => simState(c, "Blocked"), 1200); // waiting on input
      setTimeout(() => simMedia(a), 1400);
      // Ambient chatter: keep the fleet alive.
      setInterval(() => {
        if (!snapCards.length) return;
        const pick = snapCards[Math.floor(Math.random() * snapCards.length)];
        const lines = [
          "parsing 142 files…\n",
          "\x1b[33mwarn:\x1b[0m deprecation in config.rs\n",
          "indexing embeddings…\n",
          "$ git rebase origin/master\n",
          "compiling herdr-daemon v0.1.0\n",
        ];
        simOutput(pick.info.id, lines[Math.floor(Math.random() * lines.length)]);
      }, 1500);
    }, 400);
  }

  window.__HERDR_PREVIEW__ = { boot, simSpawn, simKill, simState, simOutput };
  boot(); // no-op until the app registers its conn listener (arm())
})();
