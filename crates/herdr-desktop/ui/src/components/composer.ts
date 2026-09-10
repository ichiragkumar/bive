// Composer: single send-input bar below the detail tabs (not per-tab).
// Line-send only — no raw keystroke passthrough this phase.

import type { TauriInvoke } from "../globals.js";
import type { Store } from "../store.js";
import { req } from "./shared.js";

export function mountComposer(store: Store, invoke: TauriInvoke): void {
  const input = req<HTMLInputElement>("composer");

  req("composer-send").onclick = sendLine;
  input.addEventListener("keydown", (e: KeyboardEvent) => {
    if (e.key === "Enter") sendLine();
  });

  async function sendLine(): Promise<void> {
    const id = store.state.selectedId;
    if (!id || !input.value) return;
    const text = input.value;
    input.value = "";
    await invoke("send_input_cmd", { agentId: id, text, raw: false });
    // Queue for the chat mirror so the echo classifies as a human turn.
    store.noteSentLocal(id, text);
  }
}
