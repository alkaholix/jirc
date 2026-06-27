import { useEffect, useState } from "react";
import { api, ServerProfile } from "../lib/api";
import { confirmDialog } from "../state/confirm";

interface Props {
  onClose: () => void;
  onConnect: (profile: ServerProfile) => void;
}

const DEFAULT_CHANNELS = "%#jIRC";

const BLANK: ServerProfile = {
  name: "",
  host: "irc.irc7.com",
  port: 6667,
  nick: "",
  ircx: true,
  ntlmDomain: "CG",
  tls: false,
  autoReconnect: true,
  autojoin: [],
};

// Probe the keyring once per session (avoids repeated macOS Keychain prompts).
let keyringCache: boolean | null = null;

export function ConnectDialog({ onClose, onConnect }: Props) {
  const [saved, setSaved] = useState<ServerProfile[]>([]);
  const [form, setForm] = useState<ServerProfile>({ ...BLANK });
  const [channels, setChannels] = useState(DEFAULT_CHANNELS);
  const [selected, setSelected] = useState("");
  const [keyring, setKeyring] = useState<boolean | null>(keyringCache);

  useEffect(() => {
    api.profilesLoad().then(setSaved).catch(() => {});
    if (keyringCache === null) {
      api
        .keyringAvailable()
        .then((ok) => {
          keyringCache = ok;
          setKeyring(ok);
        })
        .catch(() => setKeyring(false));
    }
  }, []);

  const load = (p: ServerProfile) => {
    setForm({ ...p });
    setChannels(p.autojoin.join(", "));
  };

  const onSelectSaved = (name: string) => {
    setSelected(name);
    const p = saved.find((s) => s.name === name);
    if (p) load(p);
  };

  const build = (): ServerProfile => ({
    ...form,
    name: form.name || form.host,
    autojoin: channels
      .split(",")
      .map((c) => c.trim())
      .filter(Boolean),
  });

  const save = async () => {
    const profile = build();
    await api.profilesSave([...saved.filter((p) => p.name !== profile.name), profile]).catch(() => {});
    // Reload so profiles have their persisted ids (needed for delete).
    const reloaded = await api.profilesLoad().catch(() => saved);
    setSaved(reloaded);
    setSelected(profile.name);
  };

  const remove = async () => {
    const p = saved.find((s) => s.name === selected);
    if (!p) return;
    const ok = await confirmDialog(`Delete saved server "${p.name}"?`, {
      title: "Delete saved server",
      confirmLabel: "Delete",
      danger: true,
    });
    if (!ok) return;
    if (p.id) await api.profilesDelete(p.id).catch(() => {});
    else await api.profilesSave(saved.filter((s) => s.name !== p.name)).catch(() => {});
    const reloaded = await api.profilesLoad().catch(() => []);
    setSaved(reloaded);
    setSelected("");
    setForm({ ...BLANK });
    setChannels(DEFAULT_CHANNELS);
  };

  const connect = async () => {
    const profile = build();
    await save();
    onConnect(profile);
    onClose();
  };

  const set = <K extends keyof ServerProfile>(k: K, v: ServerProfile[K]) =>
    setForm((f) => ({ ...f, [k]: v }));

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <h2>Add a connection</h2>
        <div className="modal-body">
          {saved.length > 0 && (
            <div className="saved-row">
              <label>
                Saved servers
                <select value={selected} onChange={(e) => onSelectSaved(e.target.value)}>
                  <option value="">— select a saved server —</option>
                  {saved.map((p) => (
                    <option key={p.name} value={p.name}>
                      {p.name}
                    </option>
                  ))}
                </select>
              </label>
              <button
                className="ghost danger-text"
                onClick={remove}
                disabled={!selected}
                title="Delete the selected saved server"
              >
                Delete
              </button>
            </div>
          )}
          <label>
            Network name
            <input value={form.name} onChange={(e) => set("name", e.target.value)} placeholder="IRC7" />
          </label>
          <div className="row">
            <label className="grow">
              Host
              <input value={form.host} onChange={(e) => set("host", e.target.value)} />
            </label>
            <label className="port">
              Port
              <input
                type="number"
                value={form.port}
                onChange={(e) => set("port", Number(e.target.value))}
              />
            </label>
          </div>
          <div className="row">
            <label className="grow">
              Nick
              <input value={form.nick} onChange={(e) => set("nick", e.target.value)} placeholder="yournick" />
            </label>
            <label className="grow">
              Alt nick
              <input
                value={form.altNick ?? ""}
                onChange={(e) => set("altNick", e.target.value)}
                placeholder="if nick is taken"
              />
            </label>
          </div>
          <label>
            Auto-join channels
            <input
              value={channels}
              onChange={(e) => setChannels(e.target.value)}
              placeholder="#chan1, #chan2"
            />
          </label>

          <div className="field-label">Authentication (optional)</div>
          <div className="row">
            <label className="grow">
              Account
              <input
                value={form.account ?? ""}
                onChange={(e) => set("account", e.target.value)}
                placeholder="defaults to nick"
              />
            </label>
            <label className="grow">
              Password
              <input
                type="password"
                value={form.accountPassword ?? ""}
                onChange={(e) => set("accountPassword", e.target.value)}
                placeholder="account password"
              />
            </label>
          </div>
          {keyring !== null && (
            <div className={`keyring-note ${keyring ? "ok" : "warn"}`}>
              {keyring
                ? "🔒 Passwords are stored in your OS keyring."
                : "⚠ OS keyring unavailable — passwords will be saved in the config file. On Linux, install a Secret Service provider (e.g. gnome-keyring)."}
            </div>
          )}

          <div className="row toggles">
            <label className="inline">
              <input
                type="checkbox"
                checked={!!form.tls}
                onChange={(e) =>
                  setForm((f) => ({
                    ...f,
                    tls: e.target.checked,
                    port: e.target.checked && f.port === 6667 ? 6697 : f.port,
                  }))
                }
              />
              TLS
            </label>
            <label className="inline" title="Skip certificate verification (self-signed servers)">
              <input
                type="checkbox"
                checked={!!form.tlsInsecure}
                onChange={(e) => set("tlsInsecure", e.target.checked)}
                disabled={!form.tls}
              />
              Insecure
            </label>
            <label className="inline">
              <input type="checkbox" checked={!!form.sasl} onChange={(e) => set("sasl", e.target.checked)} />
              SASL
            </label>
            <label className="inline">
              <input
                type="checkbox"
                checked={!!form.nickserv}
                onChange={(e) => set("nickserv", e.target.checked)}
              />
              NickServ
            </label>
            <label className="inline">
              <input
                type="checkbox"
                checked={!!form.ircx}
                onChange={(e) =>
                  setForm((f) => ({ ...f, ircx: e.target.checked, ntlm: e.target.checked ? f.ntlm : false }))
                }
              />
              IRCX
            </label>
            {form.ircx && (
              <label className="inline">
                <input type="checkbox" checked={!!form.ntlm} onChange={(e) => set("ntlm", e.target.checked)} />
                NTLM
              </label>
            )}
          </div>
          {form.ircx && form.ntlm && (
            <>
              <div className="row">
                <label className="grow">
                  NTLM domain
                  <input
                    value={form.ntlmDomain ?? ""}
                    onChange={(e) => set("ntlmDomain", e.target.value)}
                    placeholder="e.g. CG (optional)"
                  />
                </label>
                <label className="grow">
                  NTLM username
                  <input
                    value={form.ntlmUser ?? ""}
                    onChange={(e) => set("ntlmUser", e.target.value)}
                    placeholder="defaults to nick"
                  />
                </label>
              </div>
              <label>
                NTLM password
                <input
                  type="password"
                  value={form.ntlmPassword ?? ""}
                  onChange={(e) => set("ntlmPassword", e.target.value)}
                  placeholder="NTLM password"
                />
              </label>
            </>
          )}
          <div className="row toggles">
            <label className="inline">
              <input
                type="checkbox"
                checked={form.autoReconnect !== false}
                onChange={(e) => set("autoReconnect", e.target.checked)}
              />
              Auto-reconnect
            </label>
          </div>
        </div>
        <div className="modal-actions">
          <button className="ghost" onClick={save} disabled={!form.host || !form.nick}>
            Save
          </button>
          <button className="ghost" onClick={onClose}>
            Cancel
          </button>
          <button onClick={connect} disabled={!form.host || !form.nick}>
            Connect
          </button>
        </div>
      </div>
    </div>
  );
}
