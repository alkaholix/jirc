// Detachable windows: pop a buffer out into its own OS window and dock it back.
//
// A detached window runs the same bundled app in single-window mode (chosen by a
// `#win/<bufferKey>` route). It stays live off the same app-wide `irc-event`
// broadcast the main window listens to; its first paint is hydrated from a
// snapshot the popping window stashes in shared (same-origin) localStorage.
import { api } from "./api";
import { useStore, Server, Buffer } from "../state/store";

const SNAP_PREFIX = "jirc.detached.";

/** A Tauri window label for a buffer's detached window — only label-safe chars,
 *  and matching the `detached-*` capability glob. */
export const detachedLabel = (bufferKey: string) =>
  "detached-" + bufferKey.replace(/[^a-zA-Z0-9_-]/g, "_");

/** The in-app hash route a detached window loads to render just this buffer. */
export const detachedRoute = (bufferKey: string) => "win/" + encodeURIComponent(bufferKey);

/** Parses a detached-window route from a location hash, or null for the main UI. */
export function parseDetachedRoute(hash: string): string | null {
  const m = hash.match(/^#win\/(.+)$/);
  return m ? decodeURIComponent(m[1]) : null;
}

interface Snapshot {
  server: Server;
  buffer: Buffer;
}

/** Pops a buffer out: stashes a snapshot for the new window's first paint, marks
 *  it popped out in this (main) store, and spawns the OS window. */
export function popOutBuffer(bufferKey: string) {
  const s = useStore.getState();
  const buffer = s.buffers[bufferKey];
  if (!buffer) return;
  const server = s.servers[buffer.serverId];
  try {
    localStorage.setItem(SNAP_PREFIX + bufferKey, JSON.stringify({ server, buffer } as Snapshot));
  } catch {
    // Snapshot is best-effort; the window still goes live via irc-event.
  }
  s.setPoppedOut(bufferKey, true);
  api
    .openDetachedWindow(detachedLabel(bufferKey), detachedRoute(bufferKey), buffer.name)
    .catch(() => {});
}

/** Reads the snapshot a popping window stashed for this buffer (if any). */
export function readSnapshot(bufferKey: string): Snapshot | null {
  try {
    const raw = localStorage.getItem(SNAP_PREFIX + bufferKey);
    return raw ? (JSON.parse(raw) as Snapshot) : null;
  } catch {
    return null;
  }
}

/** Docks a buffer back: the backend re-shows it in the main window (via the
 *  `win-dock` broadcast) and closes the detached OS window. */
export function dockBackBuffer(bufferKey: string) {
  api.dockWindow(detachedLabel(bufferKey), bufferKey).catch(() => {});
}

/** Closes a buffer's detached window *and* the buffer (the native ✕ behaviour):
 *  the backend broadcasts `win-close-buffer` and closes the OS window. */
export function closeDetachedBuffer(bufferKey: string) {
  api.closeDetached(detachedLabel(bufferKey), bufferKey).catch(() => {});
}
