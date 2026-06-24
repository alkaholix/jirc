import { useEffect, useState } from "react";
import { api, ServerProfile } from "../lib/api";

interface Props {
  onClose: () => void;
}

/// A mIRC-style channels folder: manage the channels each saved network
/// auto-joins on connect. Edits persist to the server profiles (profiles.json),
/// which the connection task joins after `on CONNECT` (see `/autojoin`).
export function AutoJoinDialog({ onClose }: Props) {
  const [profiles, setProfiles] = useState<ServerProfile[]>([]);
  const [selected, setSelected] = useState("");
  const [channels, setChannels] = useState<string[]>([]);
  const [newChan, setNewChan] = useState("");
  const [dirty, setDirty] = useState(false);
  const [savedMsg, setSavedMsg] = useState("");

  useEffect(() => {
    api
      .profilesLoad()
      .then((ps) => {
        setProfiles(ps);
        if (ps.length > 0) {
          setSelected(ps[0].name);
          setChannels([...ps[0].autojoin]);
        }
      })
      .catch(() => {});
  }, []);

  const onSelect = (name: string) => {
    setSelected(name);
    const p = profiles.find((x) => x.name === name);
    setChannels(p ? [...p.autojoin] : []);
    setDirty(false);
    setSavedMsg("");
  };

  const add = () => {
    const raw = newChan.trim();
    if (!raw) return;
    // Bare names get a `#`; IRC/IRCX channel sigils (#&+!%) are kept as typed.
    const name = /^[#&%+!]/.test(raw) ? raw : `#${raw}`;
    setNewChan("");
    if (channels.some((c) => c.toLowerCase() === name.toLowerCase())) return;
    setChannels([...channels, name]);
    setDirty(true);
    setSavedMsg("");
  };

  const remove = (c: string) => {
    setChannels(channels.filter((x) => x !== c));
    setDirty(true);
    setSavedMsg("");
  };

  const save = async () => {
    if (!selected) return;
    const updated = profiles.map((p) =>
      p.name === selected ? { ...p, autojoin: channels } : p,
    );
    await api.profilesSave(updated).catch(() => {});
    // Reload so profiles keep their persisted ids.
    const reloaded = await api.profilesLoad().catch(() => updated);
    setProfiles(reloaded);
    setDirty(false);
    setSavedMsg("Saved");
  };

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <h2>Auto-join channels</h2>
        <div className="modal-body">
          {profiles.length === 0 ? (
            <p className="welcome-hint">
              No saved servers yet — add a connection first, then its auto-join
              channels can be managed here.
            </p>
          ) : (
            <>
              <label>
                Network
                <select value={selected} onChange={(e) => onSelect(e.target.value)}>
                  {profiles.map((p) => (
                    <option key={p.name} value={p.name}>
                      {p.name}
                    </option>
                  ))}
                </select>
              </label>
              <div className="field-label">Joined automatically on connect</div>
              <ul className="autojoin-list">
                {channels.length === 0 && <li className="empty">No channels yet.</li>}
                {channels.map((c) => (
                  <li key={c}>
                    <span>{c}</span>
                    <button
                      className="ghost danger-text"
                      onClick={() => remove(c)}
                      title={`Remove ${c}`}
                    >
                      ✕
                    </button>
                  </li>
                ))}
              </ul>
              <div className="row">
                <input
                  className="grow"
                  value={newChan}
                  onChange={(e) => setNewChan(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") add();
                  }}
                  placeholder="#channel"
                />
                <button className="ghost" onClick={add} disabled={!newChan.trim()}>
                  Add
                </button>
              </div>
            </>
          )}
        </div>
        <div className="modal-actions">
          {savedMsg && <span className="keyring-note ok">{savedMsg}</span>}
          <button className="ghost" onClick={onClose}>
            Close
          </button>
          <button onClick={save} disabled={!selected || !dirty}>
            Save
          </button>
        </div>
      </div>
    </div>
  );
}
