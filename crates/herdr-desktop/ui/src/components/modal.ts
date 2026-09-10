// Modal shell: overlay + focus trap + Esc. Hosts the spawn form and all
// confirmations so destructive actions always pass through one gate.

import { req } from "./shared.js";

export function openModalShell(title: string): { root: HTMLElement; close: () => void } {
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

  const prevFocus = document.activeElement as HTMLElement | null;
  const close = () => {
    root.hidden = true;
    root.innerHTML = "";
    document.removeEventListener("keydown", onKey, true);
    prevFocus?.focus?.();
  };
  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      e.stopPropagation();
      close();
    }
    if (e.key === "Tab") {
      // Minimal focus trap: cycle within the dialog.
      const focusables = Array.from(
        dialog.querySelectorAll<HTMLElement>("button, input, select, textarea, [tabindex]"),
      ).filter((el) => !(el as HTMLButtonElement).disabled);
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

/** Confirmation gate for destructive actions. Resolves true on confirm. */
export function confirmModal(opts: {
  title: string;
  body: string;
  confirmLabel: string;
  danger?: boolean;
}): Promise<boolean> {
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
