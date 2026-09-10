// Composer: single send-input bar below the detail tabs (not per-tab).
// Line-send only — no raw keystroke passthrough this phase.

export function mountComposer(store, invoke) {
  const input = document.getElementById("composer");

  document.getElementById("composer-send").onclick = sendLine;
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") sendLine();
  });

  async function sendLine() {
    const id = store.state.selectedId;
    if (!id || !input.value) return;
    const text = input.value;
    input.value = "";
    await invoke("send_input_cmd", { agentId: id, text, raw: false });
    // Queue for the chat mirror so the echo classifies as a human turn.
    store.noteSentLocal(id, text);
  }
}
