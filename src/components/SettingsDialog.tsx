import { useEffect, useState } from "react";
import {
  applyTheme,
  Layout,
  Theme,
  useSettings,
  type TimestampMode,
} from "../state/settings";
import { api, DataLocation, type PluginInfo } from "../lib/api";
import { dccDetect } from "../state/dcc";
import { UsersSettings } from "./UsersSettings";
import { open } from "@tauri-apps/plugin-dialog";
import { playAlertSound } from "../lib/sound";
import {
  checkForUpdate,
  installUpdate,
  type UpdateStatus,
} from "../lib/updater";
import { revealItemInDir } from "@tauri-apps/plugin-opener";

const splitList = (value: string) =>
  value
    .split(/[,\n]/)
    .map((w) => w.trim())
    .filter(Boolean);

type Tab = "appearance" | "alerts" | "behaviour" | "dcc" | "plugins" | "server" | "users";
const TABS: { id: Tab; label: string }[] = [
  { id: "appearance", label: "Appearance" },
  { id: "alerts", label: "Alerts" },
  { id: "behaviour", label: "Behaviour" },
  { id: "dcc", label: "DCC" },
  { id: "users", label: "Users" },
  { id: "plugins", label: "Plugins" },
  { id: "server", label: "Server" },
];

export function SettingsDialog({ onClose }: { onClose: () => void }) {
  const settings = useSettings();
  const [tab, setTab] = useState<Tab>("appearance");
  const [words, setWords] = useState(settings.highlightWords.join(", "));
  const [ignores, setIgnores] = useState(settings.ignores.join("\n"));
  const [notifyList, setNotifyList] = useState(settings.notifyList.join(", "));
  const [emoji, setEmoji] = useState<[string, string][]>(() =>
    Object.entries(settings.customEmoji ?? {})
  );
  const [dataLoc, setDataLoc] = useState<DataLocation | null>(null);
  const [customPath, setCustomPath] = useState("");
  const [dataMsg, setDataMsg] = useState("");
  const [dccMsg, setDccMsg] = useState("");
  const [systemFonts, setSystemFonts] = useState<string[]>([]);
  const [fontsLoading, setFontsLoading] = useState(true);
  const [updateStatus, setUpdateStatus] = useState<UpdateStatus>({ state: "idle" });
  const [plugins, setPlugins] = useState<PluginInfo[]>([]);
  const [pluginMessage, setPluginMessage] = useState("");

  const reloadPlugins = () => api.pluginsList().then(setPlugins).catch(() => setPlugins([]));

  useEffect(() => {
    api
      .dataLocation()
      .then((d) => {
        setDataLoc(d);
        setCustomPath(d.custom);
      })
      .catch(() => {});
    api
      .systemFonts()
      .then(setSystemFonts)
      .catch(() => setSystemFonts([]))
      .finally(() => setFontsLoading(false));
    void reloadPlugins();
  }, []);

  const saveDataLoc = async () => {
    await api.setDataLocation(customPath.trim() || null).catch(() => {});
    const d = await api.dataLocation().catch(() => null);
    if (d) {
      setDataLoc(d);
      setCustomPath(d.custom);
    }
    setDataMsg("Saved — restart jIRC to apply.");
  };

  const isIpv4 = (s: string) => /^\d{1,3}(\.\d{1,3}){3}$/.test(s);
  const detectDccIp = async () => {
    // A routable IPv6 (which works through CGNAT) is best; else fall back to the
    // server's view of our IPv4 (USERHOST).
    const local = await api.dccLocalIp().catch(() => "");
    if (local) {
      settings.set("dccIp", local);
      setDccMsg(`Detected ${local} (IPv6 — best for transfers across NAT)`);
      return;
    }
    const host = dccDetect.get();
    if (!host) {
      setDccMsg("Connect to a server first, then try again.");
      return;
    }
    if (isIpv4(host)) {
      settings.set("dccIp", host);
      setDccMsg(`Detected ${host}`);
      return;
    }
    const ipv4 = (await api.dnsLookup(host).catch(() => [])).find(isIpv4);
    if (ipv4) {
      settings.set("dccIp", ipv4);
      setDccMsg(`Detected ${ipv4} (${host})`);
    } else {
      setDccMsg(`Couldn't resolve ${host} — your host may be masked; enter your IP manually.`);
    }
  };

  const syncEmoji = (pairs: [string, string][]) => {
    const rec: Record<string, string> = {};
    for (const [code, value] of pairs) {
      const c = code.trim().toLowerCase();
      if (!c || !value) continue;
      rec[c.startsWith(":") ? c : `:${c}:`] = value;
    }
    settings.set("customEmoji", rec);
  };
  const editEmoji = (i: number, idx: 0 | 1, val: string) => {
    const next = emoji.map((p, j): [string, string] =>
      j !== i ? p : idx === 0 ? [val, p[1]] : [p[0], val]
    );
    setEmoji(next);
    syncEmoji(next);
  };

  const saveWords = (value: string) => {
    setWords(value);
    settings.set("highlightWords", splitList(value));
  };

  const saveIgnores = (value: string) => {
    setIgnores(value);
    settings.set("ignores", splitList(value));
  };

  const saveNotify = (value: string) => {
    setNotifyList(value);
    settings.set("notifyList", splitList(value));
  };

  const setTheme = (theme: Theme) => {
    settings.set("theme", theme);
    applyTheme(theme);
  };

  const checkUpdate = async () => {
    setUpdateStatus({ state: "checking" });
    setUpdateStatus(await checkForUpdate());
  };

  const chooseSound = async (
    key: "mentionSound" | "privateSound" | "inviteSound" | "onlineSound"
  ) => {
    const selected = await open({
      multiple: false,
      filters: [
        { name: "Audio", extensions: ["wav", "mp3", "ogg", "flac", "m4a", "aac"] },
      ],
    });
    if (typeof selected === "string") settings.set(key, selected);
  };

  const toggle = (key: Parameters<typeof settings.set>[0], label: string) => (
    <label className="inline">
      <input
        type="checkbox"
        checked={settings[key] as boolean}
        onChange={(e) => settings.set(key, e.target.checked as never)}
      />
      {label}
    </label>
  );

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal settings-modal" onClick={(e) => e.stopPropagation()}>
        <h2>Settings</h2>
        <div className="tabs">
          {TABS.map((t) => (
            <button
              key={t.id}
              className={`tab${tab === t.id ? " active" : ""}`}
              onClick={() => setTab(t.id)}
            >
              {t.label}
            </button>
          ))}
        </div>

        <div className="modal-body settings-body">
          {tab === "appearance" && (
            <>
              <div className="row">
                <label className="grow">
                  Theme
                  <select value={settings.theme} onChange={(e) => setTheme(e.target.value as Theme)}>
                    <option value="dark">Dark</option>
                    <option value="light">Light</option>
                    <option value="system">Match system</option>
                  </select>
                </label>
                <label className="grow">
                  Layout
                  <select
                    value={settings.layout}
                    onChange={(e) => settings.set("layout", e.target.value as Layout)}
                  >
                    <option value="tree">Tree (sidebar)</option>
                    <option value="switchbar">Switchbar (tabs)</option>
                  </select>
                </label>
              </div>
              <div className="setting-row">
                <div>
                  <strong>Dockable panes</strong>
                  <div className="field-help">
                    Drag pane title bars to reorder or move them between sides. Drag pane edges to resize.
                  </div>
                </div>
                <button
                  className="ghost"
                  onClick={() => {
                    settings.set("dockPaneOrder", ["treebar", "nicklist", "panels"]);
                    settings.set("dockPaneSides", { treebar: "left", nicklist: "right", panels: "right" });
                    settings.set("treebarPosition", "left");
                    settings.set("treebarWidth", 220);
                    settings.set("nicklistWidth", 180);
                    settings.set("panelsWidth", 240);
                  }}
                >
                  Reset pane layout
                </button>
              </div>
              <label className="inline color-row">
                Your nick colour
                <input
                  type="color"
                  value={settings.selfNickColor}
                  onChange={(e) => settings.set("selfNickColor", e.target.value)}
                />
                <span className="self-nick-preview" style={{ color: settings.selfNickColor }}>
                  {"<your nick>"}
                </span>
              </label>
              <label>
                Application font
                <select
                  value={settings.chatFont}
                  onChange={(e) => settings.set("chatFont", e.target.value)}
                  style={{ fontFamily: settings.chatFont || undefined }}
                  disabled={fontsLoading}
                >
                  <option value="">
                    {fontsLoading ? "Loading installed fonts…" : "Theme default"}
                  </option>
                  {settings.chatFont &&
                    !systemFonts.some(
                      (font) => font.toLowerCase() === settings.chatFont.toLowerCase()
                    ) && <option value={settings.chatFont}>{settings.chatFont}</option>}
                  {systemFonts.map((font) => (
                    <option key={font} value={font} style={{ fontFamily: font }}>
                      {font}
                    </option>
                  ))}
                </select>
              </label>
              <label className="inline">
                Application font size (px)
                <input
                  type="number"
                  min={8}
                  value={settings.chatFontSize || ""}
                  onChange={(e) =>
                    settings.set(
                      "chatFontSize",
                      e.target.value === "" ? 0 : Math.max(8, Number(e.target.value) || 8)
                    )
                  }
                  placeholder="default"
                />
              </label>
              {toggle("showInputToolbar", "Show colour and formatting input toolbar")}
              {toggle("nativePopupMenus", "Use native operating-system script popup menus")}
              <div className="row">
                {toggle("spellCheck", "Check spelling while typing messages")}
                {toggle("autoCorrect", "Auto-correct common typing mistakes")}
                <label className="grow">
                  Spelling language
                  <select
                    value={settings.spellCheckLanguage}
                    onChange={(event) =>
                      settings.set("spellCheckLanguage", event.target.value)
                    }
                    disabled={!settings.spellCheck}
                  >
                    <option value="">System language</option>
                    <option value="en-NZ">English (New Zealand)</option>
                    <option value="en-AU">English (Australia)</option>
                    <option value="en-GB">English (United Kingdom)</option>
                    <option value="en-US">English (United States)</option>
                    <option value="de">German</option>
                    <option value="es">Spanish</option>
                    <option value="fr">French</option>
                    <option value="it">Italian</option>
                    <option value="nl">Dutch</option>
                    <option value="pl">Polish</option>
                    <option value="pt-BR">Portuguese (Brazil)</option>
                    <option value="pt-PT">Portuguese (Portugal)</option>
                    <option value="sv">Swedish</option>
                  </select>
                </label>
              </div>
              <label className="inline">
                Default quit message
                <input
                  value={settings.quitMessage}
                  onChange={(e) => settings.set("quitMessage", e.target.value)}
                  placeholder="(none)"
                />
              </label>
              <label className="inline">
                Timestamps
                <select
                  value={settings.timestampMode}
                  onChange={(event) =>
                    settings.set("timestampMode", event.target.value as TimestampMode)
                  }
                >
                  <option value="inline">Timestamp · nickname · message</option>
                  <option value="divider">Timestamp divider above message</option>
                  <option value="off">Off</option>
                </select>
              </label>
              {toggle("showJoinPart", "Show join / part / quit messages")}
              <div className="emoji-editor">
                <div className="settings-label">
                  Custom emoji — <code>:code:</code> → unicode/text, or an image URL
                </div>
                {emoji.map((p, i) => (
                  <div className="row" key={i}>
                    <input
                      className="grow"
                      placeholder=":doge:"
                      value={p[0]}
                      onChange={(e) => editEmoji(i, 0, e.target.value)}
                    />
                    <input
                      className="grow"
                      placeholder="😄  or  https://…/doge.png"
                      value={p[1]}
                      onChange={(e) => editEmoji(i, 1, e.target.value)}
                    />
                    <button
                      className="ghost"
                      onClick={() => {
                        const next = emoji.filter((_, j) => j !== i);
                        setEmoji(next);
                        syncEmoji(next);
                      }}
                    >
                      ×
                    </button>
                  </div>
                ))}
                <button className="ghost" onClick={() => setEmoji([...emoji, ["", ""]])}>
                  + Add emoji
                </button>
              </div>

              <div className="css-editor">
                <div className="settings-label">
                  Custom CSS — restyle anything. Paste rules below; they apply instantly.
                </div>
                <textarea
                  className="css-area"
                  spellCheck={false}
                  value={settings.customCss}
                  onChange={(e) => settings.set("customCss", e.target.value)}
                  placeholder={":root { --accent: #ff4da6; }\n.messages { font-size: 16px; }"}
                />
                <div className="row">
                  <button className="ghost" onClick={() => settings.set("customCss", "")}>
                    Reset
                  </button>
                </div>
                <p className="cheat-tip">
                  New to CSS? The full reference, variable list and copy-paste examples
                  live in the <strong>Help</strong> (?) button.
                </p>
              </div>
            </>
          )}

          {tab === "alerts" && (
            <>
              {toggle("notifications", "Desktop notifications for mentions & PMs")}
              {toggle("soundEnabled", "Play notification sounds")}
              <label>
                Sound volume — {Math.round(settings.soundVolume * 100)}%
                <input
                  type="range"
                  min={0}
                  max={1}
                  step={0.05}
                  value={settings.soundVolume}
                  onChange={(event) => settings.set("soundVolume", Number(event.target.value))}
                />
              </label>
              <div className="sound-settings-grid">
                {([
                  ["mention", "mentionSound", "Mention"],
                  ["private", "privateSound", "Private message"],
                  ["invite", "inviteSound", "Invite"],
                  ["online", "onlineSound", "Watched user online"],
                ] as const).map(([kind, key, label]) => (
                  <div className="sound-setting" key={key}>
                    <span>{label}</span>
                    <code title={settings[key]}>{settings[key] || "Built-in tone"}</code>
                    <button onClick={() => chooseSound(key)}>Choose…</button>
                    <button className="ghost" onClick={() => settings.set(key, "")}>Default</button>
                    <button className="ghost" onClick={() => playAlertSound(kind, true)}>Test</button>
                  </div>
                ))}
              </div>
              {toggle("quietHoursEnabled", "Mute sounds during quiet hours")}
              {settings.quietHoursEnabled && (
                <div className="row">
                  <label>
                    From
                    <input
                      type="time"
                      value={settings.quietHoursFrom}
                      onChange={(event) => settings.set("quietHoursFrom", event.target.value)}
                    />
                  </label>
                  <label>
                    Until
                    <input
                      type="time"
                      value={settings.quietHoursTo}
                      onChange={(event) => settings.set("quietHoursTo", event.target.value)}
                    />
                  </label>
                </div>
              )}
              <label>
                Highlight words (comma-separated)
                <input
                  value={words}
                  onChange={(e) => saveWords(e.target.value)}
                  placeholder="keyword1, keyword2"
                />
              </label>
              <label>
                Notify list — watched nicks (comma-separated)
                <input
                  value={notifyList}
                  onChange={(e) => saveNotify(e.target.value)}
                  placeholder="friend1, friend2"
                />
              </label>
              <label>
                Ignore list (one nick or mask per line)
                <textarea
                  className="ignore-editor"
                  value={ignores}
                  spellCheck={false}
                  onChange={(e) => saveIgnores(e.target.value)}
                  placeholder={"spammer\n*!*@bad.host"}
                />
              </label>
            </>
          )}

          {tab === "behaviour" && (
            <>
              {toggle("rejoinOnKick", "Rejoin channels when kicked")}
              {toggle("rejoinOnReconnect", "Rejoin channels after a disconnect")}
              {toggle("keepOpenOnKickQuit", "Keep channel windows open on kick / disconnect")}
              {toggle("showAway", "Show when users go away / come back")}
              <div className="settings-label">Application updates</div>
              <div className="row">
                <button
                  onClick={checkUpdate}
                  disabled={
                    updateStatus.state === "checking" ||
                    updateStatus.state === "downloading"
                  }
                >
                  {updateStatus.state === "checking" ? "Checking…" : "Check for updates"}
                </button>
                {updateStatus.state === "available" && (
                  <button onClick={() => installUpdate(setUpdateStatus)}>
                    Install {updateStatus.version}
                  </button>
                )}
              </div>
              {updateStatus.state === "current" && (
                <div className="keyring-note ok">jIRC is up to date.</div>
              )}
              {updateStatus.state === "available" && (
                <div className="keyring-note ok">
                  jIRC {updateStatus.version} is available.
                  {updateStatus.notes && <div>{updateStatus.notes}</div>}
                </div>
              )}
              {updateStatus.state === "downloading" && (
                <div className="keyring-note ok">
                  Downloading {updateStatus.version}
                  {updateStatus.percent === undefined ? "…" : ` — ${updateStatus.percent}%`}
                </div>
              )}
              {updateStatus.state === "error" && (
                <div className="keyring-note warn">
                  Update check failed: {updateStatus.message}
                </div>
              )}
              <div className="settings-label">Data folder</div>
              {dataLoc && (
                <>
                  <div className="keyring-note ok">
                    Currently stored in: <code>{dataLoc.current}</code>
                  </div>
                  {dataLoc.forced ? (
                    <div className="keyring-note warn">
                      Set by the <code>JIRC_DATA_DIR</code> env var or a portable install — change
                      that to move it.
                    </div>
                  ) : (
                    <>
                      <label>
                        Custom folder (leave blank for the default, under your profile)
                        <input
                          value={customPath}
                          onChange={(e) => setCustomPath(e.target.value)}
                          placeholder="e.g. D:\jIRC-data"
                        />
                      </label>
                      <div className="row">
                        <button onClick={saveDataLoc}>Save data folder</button>
                        {dataMsg && <span className="keyring-note ok">{dataMsg}</span>}
                      </div>
                      <p className="cheat-tip">
                        Restart jIRC to apply. Existing data isn't moved automatically.
                      </p>
                    </>
                  )}
                </>
              )}
            </>
          )}

          {tab === "plugins" && (
            <>
              <div className="settings-label">Sandboxed Luau plugins</div>
              <div className="field-help">
                Plugins can receive IRC events and typed commands, then request echo, command, or notification actions. They have no filesystem, process, native, or network API.
              </div>
              <div className="row">
                <button onClick={() => api.pluginsPath().then(revealItemInDir).catch(() => {})}>Show plugins folder</button>
                <button onClick={() => api.pluginAddExample().then((path) => {
                  setPluginMessage(`Created ${path}`);
                  void reloadPlugins();
                }).catch((error) => setPluginMessage(String(error)))}>Add example plugin</button>
                <button className="ghost" onClick={reloadPlugins}>Reload</button>
              </div>
              {pluginMessage && <div className="keyring-note">{pluginMessage}</div>}
              {plugins.length === 0 ? (
                <div className="field-help">No .lua plugins are installed.</div>
              ) : plugins.map((plugin) => (
                <div className="setting-row" key={plugin.file}>
                  <div>
                    <strong>{plugin.name}</strong>
                    <div className="field-help">{plugin.file}</div>
                    {plugin.error && <div className="keyring-note error">{plugin.error}</div>}
                  </div>
                  <label className="inline">
                    <input type="checkbox" checked={plugin.enabled} disabled={!!plugin.error} onChange={(event) => {
                      void api.pluginSetEnabled(plugin.file, event.target.checked).then(reloadPlugins);
                    }} />
                    Enabled
                  </label>
                </div>
              ))}
            </>
          )}

          {tab === "dcc" && (
            <>
              <div className="settings-label">File transfers and direct chat</div>
              <label>
                Your IP for DCC (blank = automatic / local network)
                <input
                  value={settings.dccIp}
                  onChange={(e) => settings.set("dccIp", e.target.value)}
                  placeholder="e.g. your public IP, for transfers over the internet"
                />
              </label>
              <label>
                Local bind address (blank = all interfaces)
                <input
                  value={settings.dccBindIp}
                  onChange={(e) => settings.set("dccBindIp", e.target.value)}
                  placeholder="e.g. 192.168.1.20"
                />
              </label>
              <div className="row">
                <button className="ghost" onClick={detectDccIp}>
                  Detect from server
                </button>
                {dccMsg && <span className="keyring-note ok">{dccMsg}</span>}
              </div>
              <div className="row">
                <label className="grow">
                  Port from
                  <input
                    type="number"
                    min={0}
                    max={65535}
                    value={settings.dccPortFrom || ""}
                    onChange={(e) => settings.set("dccPortFrom", Number(e.target.value) || 0)}
                    placeholder="auto"
                  />
                </label>
                <label className="grow">
                  to
                  <input
                    type="number"
                    min={0}
                    max={65535}
                    value={settings.dccPortTo || ""}
                    onChange={(e) => settings.set("dccPortTo", Number(e.target.value) || 0)}
                    placeholder="auto"
                  />
                </label>
              </div>
              {toggle(
                "dccPassive",
                "Use passive/reverse DCC for outgoing chats and sends"
              )}
              <p className="cheat-tip">
                For DCC over the internet, click <strong>Detect from server</strong> —
                if you have IPv6 it'll use that (works through carrier/CGNAT, no
                port-forwarding needed). Otherwise set your public IPv4 + a port range
                and forward that range on your router. On a single LAN, leave blank.
              </p>
              <div className="settings-label">DCC Server</div>
              {toggle(
                "dccServerEnabled",
                "Listen for direct mIRC-compatible DCC Server connections"
              )}
              <label>
                Server listen port
                <input
                  type="number"
                  min={1}
                  max={65535}
                  value={settings.dccServerPort}
                  onChange={(e) =>
                    settings.set(
                      "dccServerPort",
                      Math.max(1, Math.min(65535, Number(e.target.value) || 59))
                    )
                  }
                />
              </label>
              <div className="row">
                {toggle("dccServerChat", "Chat")}
                {toggle("dccServerSend", "File sends")}
                {toggle("dccServerFserve", "Fileserver")}
              </div>
              <p className="cheat-tip">
                Port 59 is the mIRC default and may require elevated privileges on
                Unix-like systems. You can select a higher forwarded port and connect
                with <code>/dcc chat IP:port</code>.
              </p>
            </>
          )}

          {tab === "users" && <UsersSettings />}

          {tab === "server" && (
            <>
              {toggle("skipMotd", "Skip the MOTD (message of the day)")}
              {toggle("showPingPong", "Show ping? pong! events")}
              {toggle("trace", "Trace: show all raw lines & numerics in the server window")}
              <div className="settings-label">Outbound flood protection</div>
              {toggle("floodEnabled", "Rate-limit user and script output")}
              <div className="row">
                <label className="grow">
                  Messages
                  <input
                    type="number"
                    min={1}
                    max={100}
                    value={settings.floodMessages}
                    onChange={(e) =>
                      settings.set(
                        "floodMessages",
                        Math.max(1, Math.min(100, Number(e.target.value) || 1))
                      )
                    }
                  />
                </label>
                <label className="grow">
                  per seconds
                  <input
                    type="number"
                    min={1}
                    max={60}
                    value={settings.floodSeconds}
                    onChange={(e) =>
                      settings.set(
                        "floodSeconds",
                        Math.max(1, Math.min(60, Number(e.target.value) || 1))
                      )
                    }
                  />
                </label>
              </div>
              <p className="cheat-tip">
                Default: 4 messages per 2 seconds. Protocol replies and connection
                negotiation bypass this user-output queue.
              </p>
            </>
          )}
        </div>

        <div className="modal-actions">
          <button onClick={onClose}>Done</button>
        </div>
      </div>
    </div>
  );
}
