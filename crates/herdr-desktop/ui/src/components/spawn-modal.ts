// Spawn modal: first-class spawn flow (profile/host/cwd/command/args) with
// command preview, presets, and recent history. No CLI syntax memorization.

import type { TauriInvoke } from "../globals.js";
import type { Store } from "../store.js";
import { openModalShell } from "./modal.js";
import { req } from "./shared.js";
import { toast } from "./toasts.js";

interface Preset {
  label: string;
  profile: string;
  command: string;
  args: string;
}

const PRESETS: Preset[] = [
  { label: "Bash shell", profile: "bash", command: window.__HERDR_SHELL__ || "/bin/bash", args: "" },
  { label: "Generic command", profile: "generic", command: "", args: "" },
  { label: "Claude Code", profile: "claude-code", command: "claude", args: "" },
  { label: "Codex", profile: "codex", command: "codex", args: "" },
];

const RECENT_KEY = "herdr.spawn.recent";
const RECENT_KEPT = 5;

interface RecentSpawn {
  profile: string;
  host: string | null;
  cwd: string;
  command: string;
  args: string;
}

function loadRecent(): RecentSpawn[] {
  try {
    return JSON.parse(localStorage.getItem(RECENT_KEY) || "[]") as RecentSpawn[];
  } catch {
    return [];
  }
}

function saveRecent(r: RecentSpawn): void {
  const all = [r, ...loadRecent().filter((x) => JSON.stringify(x) !== JSON.stringify(r))];
  try {
    localStorage.setItem(RECENT_KEY, JSON.stringify(all.slice(0, RECENT_KEPT)));
  } catch {
    /* private mode: history just doesn't persist */
  }
}

function field(labelText: string, el: HTMLElement): HTMLElement {
  const wrap = document.createElement("label");
  wrap.className = "form-field";
  const span = document.createElement("span");
  span.textContent = labelText;
  wrap.appendChild(span);
  wrap.appendChild(el);
  return wrap;
}

export interface SpawnPrefill {
  profile?: string;
  host?: string | null;
  cwd?: string;
  command?: string;
  args?: string;
}

export function openSpawnModal(store: Store, invoke: TauriInvoke, prefill?: SpawnPrefill): void {
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
  for (const [host] of store.state.hosts) {
    const o = document.createElement("option");
    o.value = host.name;
    o.textContent = host.name;
    hostSel.appendChild(o);
  }
  hostSel.value = prefill?.host ?? store.state.selectedHost ?? "";

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
      b.textContent = `${r.profile} · ${r.command || "(cmd)"}`;
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
      const rec: RecentSpawn = {
        profile: profileSel.value,
        host: hostSel.value || null,
        cwd: cwdInput.value || "/tmp",
        command: cmdInput.value.trim(),
        args: argsInput.value.trim(),
      };
      await invoke("spawn_agent_cmd", {
        ...rec,
        args: rec.args ? rec.args.split(/\s+/) : [],
      });
      saveRecent(rec);
      // Optimistic row (J1): reconciled + auto-selected on AgentSpawned.
      store.noteSpawnOptimistic(rec.profile, rec.host);
      toast("ok", `Spawning ${rec.profile}…`);
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

export function mountSpawnEntry(store: Store, invoke: TauriInvoke): void {
  req("btn-spawn").onclick = () => openSpawnModal(store, invoke);
}
