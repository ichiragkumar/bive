// Tray mirror: the webview can't own the OS tray, so it mirrors the aggregate
// state into the document title (the Rust shell owns the real tray icon).

import type { Store } from "../store.js";
import { stateName } from "./shared.js";
import { fleetDot } from "./topbar.js";

export function mountTray(store: Store): void {
  store.subscribe((s) => {
    const dot = fleetDot(s.cards);
    const blocked = s.cards.filter((c) => stateName(c.info.state) === "Blocked").length;
    const base = "herdr — agent fleet";
    document.title = blocked ? `${base} (${blocked} need input)` : base;
    document.title += dot === "gray" ? "" : ` [${dot}]`;
  });
}
