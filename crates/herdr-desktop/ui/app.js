/* herdr desktop — event-driven UI. No polling: every update comes from a
   `herdr://event` push (or a one-shot command). */
(() => {
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;

  const fleet = document.getElementById("fleet");
  const filterSel = document.getElementById("filter");
  const connDot = document.getElementById("conn-dot");
  const summary = document.getElementById("fleet-summary");
  const detail = document.getElementById("detail");
  const detailTitle = document.getElementById("detail-title");
  const detailLog = document.getElementById("detail-log");
  const detailMedia = document.getElementById("detail-media");
  const composer = document.getElementById("composer");

  /** Agent cards as pushed by the Rust side (single source of truth). */
  let cards = [];
  let openId = null;
  /** Output activity sparkline, per agent (bucketed bytes/heartbeat). */
  const spark = new Map();
  const SPARK_MAX = 24;

  function esc(s) {
    return s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
  }

  /** Minimal ANSI → HTML for the detail log pane. */
  function ansiToHtml(text) {
    const codes = { 30: "#6b7280", 31: "#e05656", 32: "#3fb96f", 33: "#e5b34a", 34: "#4c8dff", 35: "#c084fc", 36: "#22d3ee", 37: "#e6e9f0", 90: "#8b93a5" };
    let out = "";
    let color = null;
    for (const part of text.split(/\x1b\[/)) {
      const m = part.match(/^([\d;]+)m/);
      if (!m) { out += esc(part); continue; }
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
    if ("Errored" in state) return "Errored";
    if ("Exited" in state) return "Exited";
    return Object.keys(state)[0];
  }

  function lastLine(text) {
    const lines = text.trimEnd().split("\n");
    return lines[lines.length - 1] || "";
  }

  function render() {
    const sel = filterSel.value;
    const visible = cards.filter((c) => sel === "all" || stateName(c.info.state) === sel);

    const counts = { Working: 0, Blocked: 0, Idle: 0, Errored: 0 };
    for (const c of cards) {
      const n = stateName(c.info.state);
      if (n in counts) counts[n] += 1;
    }
    const total = cards.length;
    summary.textContent = total
      ? `${total} agent${total === 1 ? "" : "s"} — ${counts.Working} working, ${counts.Blocked} blocked, ${counts.Errored} errored`
      : "no agents";
    summary.prepend(Object.assign(document.createElement("span"), { className: "dot " + fleetDot(), style: "margin-right:6px" }));

    fleet.innerHTML = "";
    if (!visible.length) {
      fleet.innerHTML = `<div class="empty">No agents. Spawn a shell or run <code>herdr spawn</code> from a terminal.</div>`;
      return;
    }
    for (const card of visible) {
      const el = document.createElement("div");
      el.className = "card";
      const id = card.info.id;
      const st = stateName(card.info.state);
      const sp = (spark.get(id) || []).map((v) => `<span style="height:${Math.max(2, v * 16)}px"></span>`).join("");
      el.innerHTML = `
        <header>
          <span class="id">${esc(id)}</span>
          <span class="badge ${st}">${esc(st)}</span>
        </header>
        <div class="last-line">${esc(lastLine(card.log_tail || "")) || "&nbsp;"}</div>
        <div class="spark">${sp}</div>`;
      el.onclick = () => openDetail(id);
      fleet.appendChild(el);
    }
  }

  function fleetDot() {
    if (!cards.length) return "gray";
    if (cards.some((c) => stateName(c.info.state) === "Errored")) return "red";
    if (cards.some((c) => stateName(c.info.state) === "Blocked")) return "amber";
    return "green";
  }

  function openDetail(id) {
    openId = id;
    const card = cards.find((c) => c.info.id === id);
    if (!card) return;
    detailTitle.textContent = `${id} · ${card.info.profile} · ${card.info.command}`;
    detailLog.innerHTML = ansiToHtml(card.log_tail || "");
    detailLog.scrollTop = detailLog.scrollHeight;
    detailMedia.innerHTML = "";
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
      detailMedia.appendChild(fig);
    }
    detail.showModal();
  }

  // ---- commands ------------------------------------------------------------
  document.getElementById("btn-spawn").onclick = async () => {
    await invoke("spawn_agent_cmd", {
      profile: "generic",
      cwd: "/tmp",
      command: window.__HERDR_SHELL__ || "/bin/bash",
      args: [],
    });
  };
  document.getElementById("btn-kill-all").onclick = () => invoke("kill_all_cmd");
  document.getElementById("detail-close").onclick = () => detail.close();
  document.getElementById("detail-kill").onclick = async () => {
    if (openId) await invoke("kill_agent_cmd", { agentId: openId });
    detail.close();
  };
  document.getElementById("detail-copy").onclick = () => {
    if (openId) navigator.clipboard.writeText(openId);
  };
  document.getElementById("composer-send").onclick = sendLine;
  composer.addEventListener("keydown", (e) => {
    if (e.key === "Enter") sendLine();
  });
  async function sendLine() {
    if (!openId || !composer.value) return;
    await invoke("send_input_cmd", { agentId: openId, text: composer.value, raw: false });
    composer.value = "";
  }
  filterSel.onchange = render;

  // ---- event subscriptions (the only update path) ---------------------------
  await listen("herdr://event", ({ payload }) => {
    const ev = payload && (payload.event || payload);
    const kind = Object.keys(ev)[0];
    const body = ev[kind];
    const id = body.agent_id || (body.info && body.info.id);

    if (kind === "AgentSpawned") {
      if (!cards.some((c) => c.info.id === id)) cards.push({ info: body.info, log_tail: "", media: [] });
      else {
        const c = cards.find((c) => c.info.id === id);
        c.info = body.info;
      }
    } else if (kind === "AgentOutput") {
      const c = cards.find((c) => c.info.id === id);
      if (c) {
        c.log_tail = ((c.log_tail || "") + body.payload).slice(-64 * 1024);
        // Sparkline: count this heartbeat.
        const arr = spark.get(id) || [];
        arr.push((arr.pop() || 0) + body.payload.length);
        if (arr.length > SPARK_MAX) arr.shift();
        spark.set(id, arr);
        // Rotate the bucket once per second of activity.
        clearTimeout(spark.get(id + ":t"));
        spark.set(id + ":t", setTimeout(() => { const a = spark.get(id) || []; if (a.at(-1) !== 0) { a.push(0); if (a.length > SPARK_MAX) a.shift(); spark.set(id, a); if (openId === id) render(); } }, 1000));
      }
    } else if (kind === "AgentMedia") {
      const c = cards.find((c) => c.info.id === id);
      if (c) {
        c.media = c.media || [];
        c.media.push({ mime: body.mime, data_base64: body.data_base64, caption: body.caption });
        c.log_tail = ((c.log_tail || "") + `[media: ${body.mime}]\n`).slice(-64 * 1024);
        if (openId === id) openDetail(id); // re-render media live
      }
    } else if (kind === "StateChange") {
      const c = cards.find((c) => c.info.id === id);
      if (c) c.info.state = body.state;
    } else if (kind === "AgentExited") {
      const c = cards.find((c) => c.info.id === id);
      if (c) c.info.state = body.code === 0 ? { Exited: 0 } : { Errored: `exit ${body.code}` };
    } else if (kind === "AgentRemoved") {
      cards = cards.filter((c) => c.info.id !== id);
      if (openId === id) detail.close();
    }
    render();
  });

  await listen("herdr://conn", ({ payload }) => {
    connDot.classList.toggle("connected", payload === "connected");
    if (payload === "connected") invoke("snapshot_cmd").then(applySnapshot);
  });

  function applySnapshot(snap) {
    if (!snap) return;
    cards = snap.cards || [];
    for (const c of cards) {
      if (!spark.has(c.info.id)) spark.set(c.info.id, []);
    }
    render();
  }

  // First paint: one snapshot, then events keep it fresh.
  invoke("snapshot_cmd").then(applySnapshot).catch(() => render());
})();
