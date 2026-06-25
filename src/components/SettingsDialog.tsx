import { useEffect, useState } from "react";
import { applyTheme, Layout, Theme, useSettings } from "../state/settings";
import { api, DataLocation } from "../lib/api";

const splitList = (value: string) =>
  value
    .split(/[,\n]/)
    .map((w) => w.trim())
    .filter(Boolean);

type Tab = "appearance" | "alerts" | "behaviour" | "server";
const TABS: { id: Tab; label: string }[] = [
  { id: "appearance", label: "Appearance" },
  { id: "alerts", label: "Alerts" },
  { id: "behaviour", label: "Behaviour" },
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

  useEffect(() => {
    api
      .dataLocation()
      .then((d) => {
        setDataLoc(d);
        setCustomPath(d.custom);
      })
      .catch(() => {});
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
      <div className="modal" onClick={(e) => e.stopPropagation()}>
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
              <label className="inline">
                Chat font
                <input
                  value={settings.chatFont}
                  onChange={(e) => settings.set("chatFont", e.target.value)}
                  placeholder="theme default"
                />
              </label>
              <label className="inline">
                Chat font size (px)
                <input
                  type="number"
                  min={0}
                  value={settings.chatFontSize || ""}
                  onChange={(e) => settings.set("chatFontSize", Number(e.target.value) || 0)}
                  placeholder="default"
                />
              </label>
              <label className="inline">
                Default quit message
                <input
                  value={settings.quitMessage}
                  onChange={(e) => settings.set("quitMessage", e.target.value)}
                  placeholder="(none)"
                />
              </label>
              {toggle("showTimestamps", "Show timestamps")}
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
              {toggle("rejoinOnKick", "Rejoin a channel when kicked")}
              {toggle("rejoinOnReconnect", "Rejoin open channels after auto-reconnect")}
              {toggle("keepOpenOnKickQuit", "Keep channel windows open on kick / disconnect")}
              {toggle("showAway", "Show when users go away / come back")}
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

          {tab === "server" && (
            <>
              {toggle("skipMotd", "Skip the MOTD (message of the day)")}
              {toggle("showPingPong", "Show ping? pong! events")}
              {toggle("trace", "Trace: show all raw lines & numerics in the server window")}
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
