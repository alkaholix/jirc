import { useEffect, useState } from "react";
import { api, AutoKind, AutoList, UserListSnapshot } from "../lib/api";

const EMPTY: UserListSnapshot = {
  entries: [],
  aop: { enabled: false, entries: [] },
  avoice: { enabled: false, entries: [] },
  protect: { enabled: false, entries: [] },
};

/** The Settings "Users" tab: the access list + auto-op/voice/protect lists. */
export function UsersSettings() {
  const [snap, setSnap] = useState<UserListSnapshot>(EMPTY);
  const refresh = () =>
    api
      .usersSnapshot()
      .then((j) => setSnap(JSON.parse(j) as UserListSnapshot))
      .catch(() => setSnap(EMPTY));
  useEffect(() => {
    refresh();
  }, []);

  const [levels, setLevels] = useState("");
  const [addr, setAddr] = useState("");
  const [info, setInfo] = useState("");
  const addUser = async () => {
    if (!levels.trim() || !addr.trim()) return;
    await api.usersSet(levels.trim(), addr.trim(), info.trim()).catch(() => {});
    setLevels("");
    setAddr("");
    setInfo("");
    refresh();
  };

  return (
    <div className="users-settings">
      <h3>Access list</h3>
      <p className="hint">
        Give nicks or address masks an access level (a number — higher means more
        access — or a name). Level-prefixed events like <code>on 10:TEXT:…</code>{" "}
        then fire only for these users. Shared across all networks.
      </p>
      <table className="users-table">
        <thead>
          <tr>
            <th>Levels</th>
            <th>Address / nick</th>
            <th>Info</th>
            <th aria-label="remove" />
          </tr>
        </thead>
        <tbody>
          {snap.entries.length === 0 && (
            <tr>
              <td colSpan={4} className="users-empty">
                No users yet.
              </td>
            </tr>
          )}
          {snap.entries.map((e) => (
            <tr key={e.address}>
              <td>{e.levels.join(", ")}</td>
              <td className="users-addr">{e.address}</td>
              <td className="users-info">{e.info}</td>
              <td>
                <button
                  className="users-x"
                  title="Remove"
                  onClick={async () => {
                    await api.usersRemove(e.address).catch(() => {});
                    refresh();
                  }}
                >
                  ✕
                </button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      <div className="users-add">
        <input
          placeholder="levels (10, or admin)"
          value={levels}
          onChange={(e) => setLevels(e.target.value)}
        />
        <input
          placeholder="*!*@host or nick"
          value={addr}
          onChange={(e) => setAddr(e.target.value)}
        />
        <input placeholder="info (optional)" value={info} onChange={(e) => setInfo(e.target.value)} />
        <button onClick={addUser}>Add</button>
      </div>

      <AutoSection title="Auto-op" kind="aop" list={snap.aop} onChange={refresh} />
      <AutoSection title="Auto-voice" kind="avoice" list={snap.avoice} onChange={refresh} />
      <AutoSection title="Protect" kind="protect" list={snap.protect} onChange={refresh} />
    </div>
  );
}

function AutoSection({
  title,
  kind,
  list,
  onChange,
}: {
  title: string;
  kind: AutoKind;
  list: AutoList;
  onChange: () => void;
}) {
  const [addr, setAddr] = useState("");
  const [chans, setChans] = useState("");
  const [net, setNet] = useState("");
  const add = async () => {
    if (!addr.trim()) return;
    const channels = chans
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);
    await api.usersAutoAdd(kind, addr.trim(), channels, net.trim()).catch(() => {});
    setAddr("");
    setChans("");
    setNet("");
    onChange();
  };
  // Group entries by network so multi-network setups read clearly.
  const byNet = new Map<string, typeof list.entries>();
  for (const e of list.entries) {
    const key = e.network || "All networks";
    (byNet.get(key) ?? byNet.set(key, []).get(key)!).push(e);
  }

  return (
    <div className="autolist">
      <div className="autolist-head">
        <h3>{title}</h3>
        <label className="inline">
          <input
            type="checkbox"
            checked={list.enabled}
            onChange={async (e) => {
              await api.usersAutoToggle(kind, e.target.checked).catch(() => {});
              onChange();
            }}
          />
          {list.enabled ? "On" : "Off"}
        </label>
      </div>
      {list.entries.length === 0 && <p className="users-empty">No entries.</p>}
      {[...byNet.entries()].map(([network, entries]) => (
        <div key={network} className="autolist-net">
          <div className="autolist-net-label">{network}</div>
          {entries.map((e) => (
            <div key={e.address} className="autolist-row">
              <span className="users-addr">{e.address}</span>
              <span className="autolist-chans">
                {e.channels.length ? e.channels.join(", ") : "all channels"}
              </span>
              <button
                className="users-x"
                title="Remove"
                onClick={async () => {
                  await api.usersAutoRemove(kind, e.address).catch(() => {});
                  onChange();
                }}
              >
                ✕
              </button>
            </div>
          ))}
        </div>
      ))}
      <div className="users-add">
        <input placeholder="*!*@host or nick" value={addr} onChange={(e) => setAddr(e.target.value)} />
        <input
          placeholder="#chan,#chan (blank = all)"
          value={chans}
          onChange={(e) => setChans(e.target.value)}
        />
        <input placeholder="network (blank = all)" value={net} onChange={(e) => setNet(e.target.value)} />
        <button onClick={add}>Add</button>
      </div>
    </div>
  );
}
