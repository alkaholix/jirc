import { Buffer, useStore } from "../state/store";
import { confirmDialog } from "../state/confirm";
import { ircxDisplay } from "../lib/ircx";

function Tab({ buffer, active }: { buffer: Buffer; active: boolean }) {
  const setActive = useStore((s) => s.setActive);
  const closeBuffer = useStore((s) => s.closeBuffer);
  const closeServer = useStore((s) => s.closeServer);
  const server = useStore((s) => s.servers[buffer.serverId]);

  const label =
    buffer.kind === "status" ? server?.name ?? "server" : ircxDisplay(buffer.name);

  return (
    <button
      className={`switch-tab${active ? " active" : ""}${buffer.mention ? " mention" : ""}`}
      onClick={() => setActive(buffer.key)}
      title={label}
    >
      {buffer.kind !== "channel" && (
        <span className="switch-icon">{buffer.kind === "query" ? "@" : buffer.kind === "window" ? "▣" : "•"}</span>
      )}
      <span className="switch-label">{label}</span>
      {buffer.unread > 0 && <span className="badge">{buffer.unread}</span>}
      <span
        className="close-x"
        title={buffer.kind === "status" ? "Close server" : "Close"}
        onClick={async (e) => {
          e.stopPropagation();
          if (buffer.kind === "status") {
            if (
              await confirmDialog(`Close ${server?.name ?? "server"} and all its windows?`, {
                title: "Close server",
                confirmLabel: "Close",
                danger: true,
              })
            )
              closeServer(buffer.serverId);
          } else {
            closeBuffer(buffer.key);
          }
        }}
      >
        ×
      </span>
    </button>
  );
}

export function SwitchBar({
  onAddServer,
  onOpenSettings,
  onOpenScripts,
  onOpenHelp,
}: {
  onAddServer: () => void;
  onOpenSettings: () => void;
  onOpenScripts: () => void;
  onOpenHelp: () => void;
}) {
  const buffers = useStore((s) => s.buffers);
  const order = useStore((s) => s.order);
  const active = useStore((s) => s.active);

  return (
    <div className="switchbar">
      <div className="switchbar-actions">
        <button className="icon-btn" onClick={onOpenHelp} title="Help">
          ?
        </button>
        <button className="icon-btn" onClick={onOpenScripts} title="Scripts">
          ⟨⟩
        </button>
        <button className="icon-btn" onClick={onOpenSettings} title="Settings">
          ⚙
        </button>
        <button className="icon-btn" onClick={onAddServer} title="Add a connection">
          +
        </button>
      </div>
      <div className="switchbar-tabs">
        {order.map((key) => {
          const b = buffers[key];
          return b ? <Tab key={key} buffer={b} active={key === active} /> : null;
        })}
      </div>
    </div>
  );
}
