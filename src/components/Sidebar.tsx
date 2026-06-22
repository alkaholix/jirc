import { useState } from "react";
import { Buffer, Server, useStore } from "../state/store";
import { confirmDialog } from "../state/confirm";
import { ircxDisplay } from "../lib/ircx";
import { useNotify } from "../state/notify";

function BufferItem({ buffer, active }: { buffer: Buffer; active: boolean }) {
  const setActive = useStore((s) => s.setActive);
  const closeBuffer = useStore((s) => s.closeBuffer);
  const label = buffer.kind === "status" ? "Server console" : ircxDisplay(buffer.name);
  return (
    <div
      className={`buffer-item${active ? " active" : ""}${buffer.mention ? " mention" : ""}`}
      onClick={() => setActive(buffer.key)}
    >
      {buffer.kind !== "channel" && (
        <span className={`buffer-icon ${buffer.kind}`}>
          {buffer.kind === "query" ? "@" : "•"}
        </span>
      )}
      <span className="buffer-name">{label}</span>
      {buffer.unread > 0 && <span className="badge">{buffer.unread}</span>}
      {buffer.kind !== "status" && (
        <button
          className="close-x"
          title="Close"
          onClick={(e) => {
            e.stopPropagation();
            closeBuffer(buffer.key);
          }}
        >
          ×
        </button>
      )}
    </div>
  );
}

function ServerGroup({ server, buffers }: { server: Server; buffers: Buffer[] }) {
  const active = useStore((s) => s.active);
  const closeServer = useStore((s) => s.closeServer);
  const [collapsed, setCollapsed] = useState(false);

  return (
    <div className="server-group">
      <div className="server-name">
        <button className="caret" onClick={() => setCollapsed((c) => !c)} title="Collapse">
          {collapsed ? "▸" : "▾"}
        </button>
        <span className={`dot${server.connected ? " on" : ""}`} title={server.connected ? "connected" : "disconnected"} />
        <span className="server-label">{server.name}</span>
        <button
          className="close-x server-close"
          title="Disconnect & close this server"
          onClick={async () => {
            if (
              await confirmDialog(`Close ${server.name} and all its windows?`, {
                title: "Close server",
                confirmLabel: "Close",
                danger: true,
              })
            )
              closeServer(server.id);
          }}
        >
          ⏻
        </button>
      </div>
      {!collapsed &&
        buffers.map((b) => <BufferItem key={b.key} buffer={b} active={b.key === active} />)}
    </div>
  );
}

export function Sidebar({
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
  const servers = useStore((s) => s.servers);
  const buffers = useStore((s) => s.buffers);
  const order = useStore((s) => s.order);
  const onlineMap = useNotify((s) => s.online);
  const friends = [...new Set(Object.values(onlineMap).flat())].sort();

  const byServer: Record<string, Buffer[]> = {};
  for (const key of order) {
    const b = buffers[key];
    if (!b) continue;
    (byServer[b.serverId] ??= []).push(b);
  }

  return (
    <nav className="sidebar">
      <div className="sidebar-header">
        <span>jIRC</span>
        <div className="header-actions">
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
      </div>
      <div className="sidebar-body">
        {Object.values(servers).map((srv) => (
          <ServerGroup key={srv.id} server={srv} buffers={byServer[srv.id] ?? []} />
        ))}
        {Object.keys(servers).length === 0 && (
          <div className="empty-hint">No connections yet. Click + to add one.</div>
        )}
      </div>
      {friends.length > 0 && (
        <div className="notify-panel">
          <div className="notify-title">Online · {friends.length}</div>
          {friends.map((n) => (
            <div key={n} className="notify-nick">
              <span className="dot on" /> {n}
            </div>
          ))}
        </div>
      )}
    </nav>
  );
}
