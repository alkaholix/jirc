import { Buffer, useStore } from "../state/store";
import { parseIrc } from "../ircFormat/parse";
import { api } from "../lib/api";
import { ircxDisplay } from "../lib/ircx";
import { useAway } from "../state/away";
import { promptDialog } from "../state/prompt";

export function TopicBar({
  buffer,
  onPopOut,
  onDock,
}: {
  buffer: Buffer;
  onPopOut?: () => void;
  onDock?: () => void;
}) {
  const server = useStore((s) => s.servers[buffer.serverId]);
  const away = useAway((s) => !!s.away[buffer.serverId]);
  const title = buffer.kind === "status" ? server?.name ?? "Server" : ircxDisplay(buffer.name);

  const toggleAway = async () => {
    if (away) {
      api.sendRaw(buffer.serverId, "AWAY").catch(() => {});
    } else {
      const reason = await promptDialog("Set your away message:", {
        title: "Away",
        initial: "Away",
        placeholder: "Away",
        confirmLabel: "Set away",
      });
      if (reason !== null) {
        api.sendRaw(buffer.serverId, `AWAY :${reason || "Away"}`).catch(() => {});
      }
    }
  };

  const editTopic = async () => {
    if (buffer.kind !== "channel") return;
    const next = await promptDialog(`Set the topic for ${ircxDisplay(buffer.name)}:`, {
      title: "Channel topic",
      initial: buffer.topic ?? "",
      confirmLabel: "Set topic",
    });
    if (next !== null) {
      api.sendRaw(buffer.serverId, `TOPIC ${buffer.name} :${next}`).catch(() => {});
    }
  };

  return (
    <header className="topicbar">
      <div className="topic-title">
        {title}
        {buffer.kind === "channel" && <span className="member-count"> · {buffer.members.length}</span>}
      </div>
      <div
        className="topic-text"
        title={buffer.kind === "channel" ? "Double-click to edit topic" : undefined}
        onDoubleClick={editTopic}
      >
        {buffer.topic ? parseIrc(buffer.topic) : buffer.kind === "channel" ? <span className="topic-empty">(no topic — double-click to set)</span> : null}
      </div>
      <button
        className={`away-btn${away ? " on" : ""}`}
        onClick={toggleAway}
        title={away ? "You are away — click to return" : "Set yourself away"}
      >
        {away ? "● Away" : "Away"}
      </button>
      {onDock && (
        <button className="win-btn" onClick={onDock} title="Dock this window back into jIRC">
          ⧈ Dock
        </button>
      )}
      {onPopOut && (
        <button className="win-btn" onClick={onPopOut} title="Pop out into its own window">
          ⧉
        </button>
      )}
    </header>
  );
}
