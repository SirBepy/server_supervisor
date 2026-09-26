// Host-window geometry for dock-pane.ts: converts a pane's CSS-px rect into
// the screen-physical pixels the backend's window-positioning API expects.
//
// getBoundingClientRect() returns CSS px relative to the webview's own
// viewport (its client area). The backend positions windows in SCREEN
// PHYSICAL pixels. Converting one to the other needs two numbers Tauri's
// window API can give us but the DOM can't: the client area's own
// top-left corner in screen space (innerPosition, already physical px - NOT
// innerPosition/devicePixelRatio, since Tauri already returns it in the
// physical space Win32 wants), and the per-window scale factor. That scale
// factor is read from Tauri (`scaleFactor()`), not `window.devicePixelRatio`
// - both usually agree on Windows, but Tauri's figure is the one the Rust
// side is guaranteed to also be using, so it's the source of truth here.
//
// Cached and refreshed on window move/resize plus every poll tick as a
// backstop; a resize/scroll of the PANE itself only changes the pane's rect
// within the window, not the window's own screen origin, so a tick-stale
// cache here is not a correctness bug on its own - only dragging the host
// window in the same instant as a pane layout change could read one tick
// stale, and it self-corrects next tick.
//
// This module is the ONLY owner of winOriginX/Y, winScale and geometryReady.
// dock-pane.ts never reads or writes them directly - it goes through
// refreshWindowGeometry/isGeometryReady/computeRect/ensureGeometryListeners
// below, so there is exactly one copy of this cache.

import { getCurrentWindow } from "@tauri-apps/api/window";
import type { DockRect } from "../../types/ipc.generated";

let winOriginX = 0;
let winOriginY = 0;
let winScale = 1;
let geometryReady = false;

export function isGeometryReady(): boolean {
  return geometryReady;
}

export async function refreshWindowGeometry(): Promise<void> {
  try {
    const win = getCurrentWindow();
    const [pos, scale] = await Promise.all([win.innerPosition(), win.scaleFactor()]);
    winOriginX = pos.x;
    winOriginY = pos.y;
    winScale = scale;
    geometryReady = true;
  } catch {
    // Leave the previous cached values in place; a transient IPC hiccup here
    // shouldn't make every dock rect jump to (0,0).
  }
}

let geometryListenersAttached = false;
// `afterResize` lets the caller re-assert its own docked panes once the
// window's new origin/scale have resolved, without this module needing to
// know anything about dock-pane.ts's per-command state.
export function ensureGeometryListeners(afterResize: () => void) {
  if (geometryListenersAttached) return;
  geometryListenersAttached = true;
  void refreshWindowGeometry();
  const win = getCurrentWindow();
  void win.onMoved(() => void refreshWindowGeometry());
  void win.onResized(() => void refreshWindowGeometry().then(afterResize));
}

export function computeRect(el: HTMLElement): DockRect {
  const r = el.getBoundingClientRect();
  return {
    left: Math.round(winOriginX + r.left * winScale),
    top: Math.round(winOriginY + r.top * winScale),
    right: Math.round(winOriginX + r.right * winScale),
    bottom: Math.round(winOriginY + r.bottom * winScale),
  };
}
