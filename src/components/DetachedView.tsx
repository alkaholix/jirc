import { useEffect } from "react";
import { useStore } from "../state/store";
import { readSnapshot, dockBackBuffer } from "../lib/detach";
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

  useEffect(() => {
    const snap = readSnapshot(bufferKey);
    if (snap) addDetachedBuffer(snap.server, snap.buffer);
  }, [bufferKey, addDetachedBuffer]);

  if (!buffer) {
    return (
      <div className="app detached">
        <main className="main">
          <div className="welcome">
            <p>Loading window…</p>
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
