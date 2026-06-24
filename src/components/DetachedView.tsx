import { useEffect, useRef } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useStore } from "../state/store";
import { readSnapshot, dockBackBuffer, closeDetachedBuffer } from "../lib/detach";
import { TopicBar } from "./TopicBar";
import { MessageList } from "./MessageList";
import { NickList } from "./NickList";
import { InputBar } from "./InputBar";

/** Single-window mode: renders just one buffer in its own OS window. Kept live by
 *  the same app-wide `irc-event` broadcast the main window listens to; its first
 *  paint is hydrated from the snapshot the popping window stashed. */
export function DetachedView({ bufferKey }: { bufferKey: string }) {
  const addDetachedBuffer = useStore((s) => s.addDetachedBuffer);
  const buffer = useStore((s) => s.buffers[bufferKey] ?? null);
  // Set when we close the window ourselves (dock-back or ✕) so the OS
  // close-request handler doesn't double-handle it.
  const handledClose = useRef(false);
  // Set once the buffer has appeared, to tell "still loading" from "was closed".
  const everLoaded = useRef(false);
  if (buffer) everLoaded.current = true;

  useEffect(() => {
    const snap = readSnapshot(bufferKey);
    if (snap) addDetachedBuffer(snap.server, snap.buffer);
  }, [bufferKey, addDetachedBuffer]);

  // Native ✕ closes the buffer itself (not just this window). Dock-back sets
  // handledClose first, so it keeps the buffer instead.
  useEffect(() => {
    const win = getCurrentWindow();
    const unlisten = win.onCloseRequested((event) => {
      if (handledClose.current) return;
      handledClose.current = true;
      event.preventDefault(); // the backend closes the window after the buffer
      closeDetachedBuffer(bufferKey);
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, [bufferKey]);

  const dockBack = () => {
    handledClose.current = true; // keep the buffer; just re-dock it
    dockBackBuffer(bufferKey);
  };

  const closeWindow = () => {
    handledClose.current = true;
    closeDetachedBuffer(bufferKey);
  };

  if (!buffer) {
    return (
      <div className="app detached">
        <main className="main">
          <div className="welcome">
            {everLoaded.current ? (
              <>
                <h1>Window closed</h1>
                <p>This conversation is no longer open in jIRC.</p>
                <div className="welcome-actions">
                  <button onClick={closeWindow}>Close window</button>
                </div>
              </>
            ) : (
              <p>Loading window…</p>
            )}
          </div>
        </main>
      </div>
    );
  }

  return (
    <div className="app detached">
      <main className="main">
        <TopicBar buffer={buffer} onDock={dockBack} />
        <div className="main-body">
          <div className="chat-pane">
            <MessageList buffer={buffer} />
            <InputBar buffer={buffer} />
          </div>
          {buffer.kind === "channel" && <NickList buffer={buffer} />}
        </div>
      </main>
    </div>
  );
}
