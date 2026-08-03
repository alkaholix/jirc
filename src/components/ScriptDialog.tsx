import { useEffect, useMemo, useState } from "react";
import { api } from "../lib/api";
import { MslEditor } from "./MslEditor";
import { mslDiagnostics } from "../lib/mslLanguage";
import { SCRIPT_THEMES } from "../lib/mslLanguage";
import { useSettings, type ScriptTheme } from "../state/settings";

const DRAFT_PREFIX = "jirc.script-draft.";

type EditorSection = "aliases" | "popups" | "remote";

const ALIASES_EXAMPLE = `; Aliases — create your own /commands here.
; Type /hello in a channel after saving.
alias hello {
  /echo -a Hello $me $+ !
}

; Parameters are available as $1, $2, and $1-.
alias greet {
  /msg $chan Welcome $1 $+ !
}
`;

const POPUPS_EXAMPLE = `; Popups — customise jIRC's right-click menus.
; Contexts: nicklist, channel, query, status, or a custom @window name.
menu nicklist {
  Whois:/whois $1
  Query:/query $1
  -
  Control
  .Op:/mode $chan +o $1
  .Voice:/mode $chan +v $1
  .Kick:/kick $chan $1
}

menu channel {
  Channel modes:/channel $chan
  -
  Part:/part $chan
}

menu query {
  Close:/close
}
`;

const EXAMPLE = `; jIRC script (mSL subset)
; Type /hello in a channel
alias hello {
  /msg $chan Hello from a script, $me $+ !
}

; Auto-reply to !ping in any channel
on *:TEXT:!ping*:#:{
  /msg $chan pong $nick
}

; Greet people who join your channels
on *:JOIN:#:{
  /msg $chan welcome $nick
}

; Timers: /timer <reps> <seconds> <command>
alias countdown {
  /timer 3 1 /msg $chan tick $+ ...
}

; Customise the nick-list right-click menu ($1 = selected nick).
; Leading dots make submenus; a line with just - is a separator.
menu nicklist {
  Whois:/whois $1
  -
  Control
  .Op:/mode $chan +o $1
  .Deop:/mode $chan -o $1
  .Kick:/kick $chan $1
  -
  Slap:/me slaps $1 around a bit
}
`;

export function ScriptDialog({
  onClose,
  standalone = false,
}: {
  onClose: () => void;
  standalone?: boolean;
}) {
  const [names, setNames] = useState<string[]>([]);
  const [section, setSection] = useState<EditorSection>("aliases");
  const [current, setCurrent] = useState<string | null>(null);
  const [source, setSource] = useState("");
  const [status, setStatus] = useState("");
  const [dirty, setDirty] = useState(false);
  const scriptTheme = useSettings((state) => state.scriptTheme);
  const setSetting = useSettings((state) => state.set);
  const diagnostics = useMemo(() => mslDiagnostics(source), [source]);

  const select = async (name: string) => {
    const text = await api.scriptRead(name).catch(() => "");
    const draft = localStorage.getItem(DRAFT_PREFIX + name);
    setCurrent(name);
    setSource(draft ?? text);
    setDirty(draft !== null && draft !== text);
  };

  const refresh = async (selectName?: string, requestedSection: EditorSection = section) => {
    const list = await api.scriptsList().catch((): string[] => []);
    setNames(list);
    if (requestedSection === "aliases" || requestedSection === "popups") {
      const name = requestedSection;
      const text = await api.scriptRead(name).catch(() => "");
      const draft = localStorage.getItem(DRAFT_PREFIX + name);
      const fallback = requestedSection === "aliases" ? ALIASES_EXAMPLE : POPUPS_EXAMPLE;
      setCurrent(name);
      setSource(draft ?? (text || fallback));
      setDirty(draft !== null || !list.includes(name));
      return;
    }
    const remoteNames = list.filter((name) => name !== "aliases" && name !== "popups");
    const pick = selectName ?? (current && remoteNames.includes(current) ? current : null) ?? remoteNames[0] ?? null;
    if (pick && remoteNames.includes(pick)) {
      void select(pick);
    } else {
      setCurrent(null);
      setSource("");
    }
  };

  useEffect(() => {
    refresh(undefined, "aliases");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const newScript = async () => {
    const name = window.prompt("New script name:", "myscript");
    if (!name) return;
    await api.scriptWrite(name, EXAMPLE).catch(() => {});
    setSection("remote");
    await refresh(name, "remote");
    setStatus(`Created ${name}`);
  };

  const addExamples = async () => {
    const added = await api.scriptAddExamples().catch(() => 0);
    await refresh();
    setStatus(added > 0 ? `Added ${added} example script(s)` : "Examples already present");
    setTimeout(() => setStatus(""), 2500);
  };

  const changeSection = (next: EditorSection) => {
    setSection(next);
    setStatus("");
    void refresh(undefined, next);
  };

  const save = async () => {
    if (!current) return;
    try {
      await api.scriptWrite(current, source);
      localStorage.removeItem(DRAFT_PREFIX + current);
      setDirty(false);
      setStatus("Saved & compiled ✓");
      setTimeout(() => setStatus(""), 2000);
    } catch (e) {
      setStatus(`Error: ${e}`);
    }
  };

  const remove = async () => {
    if (!current) return;
    if (!confirm(`Delete script "${current}"?`)) return;
    await api.scriptDelete(current).catch(() => {});
    localStorage.removeItem(DRAFT_PREFIX + current);
    setCurrent(null);
    await refresh();
  };

  return (
    <div
      className={standalone ? "script-window" : "modal-backdrop"}
      onClick={standalone ? undefined : onClose}
    >
      <div
        className={standalone ? "script-modal standalone" : "modal script-modal"}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="script-titlebar">
          <h2>Scripts (mSL)</h2>
          <label className="script-theme">
            Editor theme
            <select
              value={scriptTheme}
              onChange={(event) =>
                setSetting("scriptTheme", event.target.value as ScriptTheme)
              }
            >
              {SCRIPT_THEMES.map((theme) => (
                <option key={theme.value} value={theme.value}>
                  {theme.label}
                </option>
              ))}
            </select>
          </label>
          {!standalone && (
            <button
              className="ghost"
              onClick={() => {
                if (current && dirty) localStorage.setItem(DRAFT_PREFIX + current, source);
                api.openScriptEditor().then(onClose).catch(() => {});
              }}
              title="Open the script editor in a separate resizable window"
            >
              ⧉ Pop out
            </button>
          )}
        </div>
        <div className="script-tabs" role="tablist" aria-label="Script editor sections">
          {(["aliases", "popups", "remote"] as const).map((tab) => (
            <button
              key={tab}
              role="tab"
              aria-selected={section === tab}
              className={section === tab ? "active" : ""}
              onClick={() => changeSection(tab)}
            >
              {tab === "aliases" ? "Aliases" : tab === "popups" ? "Popups" : "Remote"}
            </button>
          ))}
        </div>
        <p className="script-hint">
          {section === "aliases" ? (
            <>Create custom slash commands in <code>scripts/aliases.mrc</code>.</>
          ) : section === "popups" ? (
            <>Create right-click menus in <code>scripts/popups.mrc</code>. Nested items begin with a dot; <code>-</code> adds a separator.</>
          ) : (
            <>Remote scripts live in <code>scripts/*.mrc</code>. All files are compiled together and can share aliases, events and variables.</>
          )}
        </p>
        <div className="script-layout">
          {section === "remote" && <div className="script-list">
            {names.filter((name) => name !== "aliases" && name !== "popups").map((n) => (
              <button
                key={n}
                className={`script-list-item${n === current ? " active" : ""}`}
                onClick={() => select(n)}
              >
                {n}
              </button>
            ))}
            {names.filter((name) => name !== "aliases" && name !== "popups").length === 0 && <div className="empty-hint">No remote scripts yet.</div>}
            <button className="script-new" onClick={newScript}>
              + New script
            </button>
            <button className="script-new" onClick={addExamples} title="Add bundled example scripts">
              + Examples
            </button>
          </div>}
          <div className="script-editor-pane">
            {current ? (
              <MslEditor
                value={source}
                onChange={(value) => {
                  setSource(value);
                  setDirty(true);
                  localStorage.setItem(DRAFT_PREFIX + current, value);
                }}
                onSave={save}
                theme={scriptTheme}
              />
            ) : (
              <div className="script-placeholder">Select a script, or create a new one.</div>
            )}
          </div>
        </div>
        <div className="modal-actions">
          <span className="script-status">{status}</span>
          {diagnostics.length > 0 && (
            <span className="script-diagnostics">
              {diagnostics.filter((item) => item.severity === "error").length} errors,{" "}
              {diagnostics.filter((item) => item.severity === "warning").length} warnings
            </span>
          )}
          {current && section === "remote" && (
            <button className="ghost danger-text" onClick={remove}>
              Delete
            </button>
          )}
          <button className="ghost" onClick={onClose}>
            {standalone ? "Close window" : "Close"}
          </button>
          <button onClick={save} disabled={!current || !dirty}>
            Save &amp; compile
          </button>
        </div>
      </div>
    </div>
  );
}
