// Composer: single send-input bar below the detail tabs (not per-tab).
// Line-send only — no raw keystroke passthrough this phase.

import type { TauriInvoke } from "../globals.js";
import type { Store } from "../store.js";
import { req, stateName } from "./shared.js";

export function mountComposer(store: Store, invoke: TauriInvoke): void {
  const input = req<HTMLTextAreaElement>("composer");
  const raw = req<HTMLInputElement>("composer-raw");
  const bar = req("composer-bar");
  const warn = req("composer-warn");
  const sendBtn = req<HTMLButtonElement>("composer-send");

  req("composer-send").onclick = sendLine;
  input.addEventListener("keydown", (e: KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey) {
      // Single-line send; Shift+Enter = newline, ⌘/Ctrl+Enter = send.
      e.preventDefault();
      sendLine();
    }
  });

  async function sendLine(): Promise<void> {
    const id = store.state.selectedId;
    if (!id || !input.value) return;
    const text = input.value;
    const useRaw = raw.checked;
    input.value = "";
    warn.hidden = true;
    try {
      await invoke("send_input_cmd", { agentId: id, text, raw: useRaw });
      // Queue for the chat mirror so the echo classifies as a human turn.
      store.noteSentLocal(id, text);
    } catch (e) {
      warn.textContent = `Send failed: ${String(e)}`;
      warn.hidden = false;
    }
  }

  store.subscribe((s) => {
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
      warn.textContent = `Agent ${st === "Exited" ? "exited" : "errored"} — sending is disabled.`;
      warn.hidden = false;
    } else warn.hidden = true;
  });
}
