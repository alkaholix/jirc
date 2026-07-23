import { useEffect, useMemo, useState } from "react";
import { api } from "../lib/api";
import { MslEditor } from "./MslEditor";
import { mslDiagnostics } from "../lib/mslLanguage";

const DRAFT_PREFIX = "jirc.script-draft.";

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
  const [current, setCurrent] = useState<string | null>(null);
  const [source, setSource] = useState("");
  const [status, setStatus] = useState("");
  const [dirty, setDirty] = useState(false);
  const diagnostics = useMemo(() => mslDiagnostics(source), [source]);

  const select = async (name: string) => {
    const text = await api.scriptRead(name).catch(() => "");
    const draft = localStorage.getItem(DRAFT_PREFIX + name);
    setCurrent(name);
    setSource(draft ?? text);
    setDirty(draft !== null && draft !== text);
  };

  const refresh = async (selectName?: string) => {
    const list = await api.scriptsList().catch((): string[] => []);
    setNames(list);
    const pick = selectName ?? current ?? list[0] ?? null;
    if (pick && list.includes(pick)) {
      void select(pick);
    } else if (list.length === 0) {
      setCurrent(null);
      setSource("");
    }
  };

  useEffect(() => {
    refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const newScript = async () => {
    const name = window.prompt("New script name:", "myscript");
    if (!name) return;
    await api.scriptWrite(name, EXAMPLE).catch(() => {});
    await refresh(name);
    setStatus(`Created ${name}`);
  };

  const addExamples = async () => {
    const added = await api.scriptAddExamples().catch(() => 0);
    await refresh();
    setStatus(added > 0 ? `Added ${added} example script(s)` : "Examples already present");
    setTimeout(() => setStatus(""), 2500);
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
        <p className="script-hint">
          Multiple script files live in <code>scripts/*.mrc</code> and are all compiled
          together. Aliases, events, <code>if</code>/<code>while</code>, <code>%vars</code>,
          hash tables, <code>/timer</code>, and identifiers like <code>$nick</code>,{" "}
          <code>$chan</code>, <code>$rand()</code>, <code>$calc()</code>.
        </p>
        <div className="script-layout">
          <div className="script-list">
            {names.map((n) => (
              <button
                key={n}
                className={`script-list-item${n === current ? " active" : ""}`}
                onClick={() => select(n)}
              >
                {n}
              </button>
            ))}
            {names.length === 0 && <div className="empty-hint">No scripts yet.</div>}
            <button className="script-new" onClick={newScript}>
              + New script
            </button>
            <button className="script-new" onClick={addExamples} title="Add bundled example scripts">
              + Examples
            </button>
          </div>
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
          {current && (
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
