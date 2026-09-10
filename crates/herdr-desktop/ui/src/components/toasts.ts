// Toasts: top-right transient feedback for every async action. No silent
// spinners, no silent failures.

export type ToastKind = "ok" | "err" | "info";

export function toast(kind: ToastKind, text: string, ms = 4000): void {
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

export function toastCopy(btn: HTMLButtonElement, doneLabel = "copied ✓"): void {
  const orig = btn.textContent;
  btn.textContent = doneLabel;
  setTimeout(() => {
    btn.textContent = orig;
  }, 1200);
}
