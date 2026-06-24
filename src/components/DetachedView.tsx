import { useEffect, useRef } from "react";
import { useStore } from "../state/store";
import { readSnapshot, dockBackBuffer, closeDetachedBuffer } from "../lib/detach";
import { TopicBar } from "./TopicBar";
import { MessageList } from "./MessageList";
import { NickList } from "./NickList";
import { InputBar } from "./InputBar";

/** Single-window mode: renders just one buffer in its own OS window. Kept live by
 *  the same app-wide `irc-event` broadcast the main window listens to; its first
 *  paint is hydrated from the snapshot the popping window stashed.
 *
 *  Window close is owned by the backend: `dock_window` / `close_detached` emit the
 *  right buffer action and then close the window, and the native ✕ just closes the
 *  window. There's no JS close interception here — intercepting it deadlocked the
 *  window and mis-fired for closes triggered from the main window. */
export function DetachedView({ bufferKey }: { bufferKey: string }) {
  const addDetachedBuffer = useStore((s) => s.addDetachedBuffer);
  const buffer = useStore((s) => s.buffers[bufferKey] ?? null);
  // Set once the buffer has appeared, to tell "still loading" from "was closed".
  const everLoaded = useRef(false);
  if (buffer) everLoaded.current = true;

  useEffect(() => {
    const snap = readSnapshot(bufferKey);
    if (snap) addDetachedBuffer(snap.server, snap.buffer);
  }, [bufferKey, addDetachedBuffer]);

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
                  <button onClick={() => closeDetachedBuffer(bufferKey)}>Close window</button>
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
        <TopicBar buffer={buffer} onDock={() => dockBackBuffer(bufferKey)} />
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
