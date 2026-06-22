import { useMemo, useState } from "react";
import { useStore } from "../state/store";
import { api } from "../lib/api";
import { parseIrc } from "../ircFormat/parse";
import { ircxDisplay } from "../lib/ircx";

type SortKey = "users" | "channel";

export function ChannelListDialog() {
  const list = useStore((s) => s.channelList);
  const close = useStore((s) => s.closeChannelList);
  const open = useStore((s) => s.openChannelList);
  const server = useStore((s) => (list ? s.servers[list.serverId] : undefined));
  const [filter, setFilter] = useState("");
  const [sort, setSort] = useState<SortKey>("users");

  const rows = useMemo(() => {
    if (!list) return [];
    const f = filter.trim().toLowerCase();
    const filtered = f
      ? list.entries.filter(
          (e) => e.channel.toLowerCase().includes(f) || e.topic.toLowerCase().includes(f)
        )
      : list.entries;
    const sorted = [...filtered].sort((a, b) =>
      sort === "users" ? b.users - a.users : a.channel.localeCompare(b.channel)
    );
    return sorted.slice(0, 2000);
  }, [list, filter, sort]);

  if (!list) return null;

  const join = (channel: string) => {
    api.join(list.serverId, channel).catch(() => {});
  };

  const refresh = () => {
    open(list.serverId);
    api.sendRaw(list.serverId, "LIST").catch(() => {});
  };

  return (
    <div className="modal-backdrop" onClick={close}>
      <div className="modal list-modal" onClick={(e) => e.stopPropagation()}>
        <h2>Channel list — {server?.name ?? "server"}</h2>
        <div className="list-toolbar">
          <input
            autoFocus
            placeholder="Filter channels or topics…"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
          />
          <select value={sort} onChange={(e) => setSort(e.target.value as SortKey)}>
            <option value="users">Sort by users</option>
            <option value="channel">Sort by name</option>
          </select>
          <button className="ghost" onClick={refresh}>
            Refresh
          </button>
        </div>
        <div className="list-count">
          {list.loading ? "Loading… " : ""}
          {list.entries.length} channels
          {rows.length < list.entries.length ? ` (${rows.length} shown)` : ""}
        </div>
        <div className="list-table">
          <div className="list-row list-head">
            <span className="lc-name">Channel</span>
            <span className="lc-users">Users</span>
            <span className="lc-topic">Topic</span>
          </div>
          {rows.map((e) => (
            <div
              key={e.channel}
              className="list-row"
              title={`Double-click to join ${ircxDisplay(e.channel)}`}
              onDoubleClick={() => join(e.channel)}
            >
              <span className="lc-name">{ircxDisplay(e.channel)}</span>
              <span className="lc-users">{e.users}</span>
              <span className="lc-topic">{parseIrc(e.topic)}</span>
              <button className="lc-join" onClick={() => join(e.channel)}>
                Join
              </button>
            </div>
          ))}
          {!list.loading && list.entries.length === 0 && (
            <div className="empty-hint">No channels returned.</div>
          )}
        </div>
        <div className="modal-actions">
          <button onClick={close}>Close</button>
        </div>
      </div>
    </div>
  );
}
