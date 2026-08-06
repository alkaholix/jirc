import { useEffect, useMemo, useState } from "react";
import { api } from "../lib/api";
import { MslEditor } from "./MslEditor";
import { mslDiagnostics } from "../lib/mslLanguage";
import { SCRIPT_THEMES } from "../lib/mslLanguage";
import { useSettings, type ScriptTheme } from "../state/settings";

const DRAFT_PREFIX = "jirc.script-draft.";
const POPUP_INIT_KEY = "jirc.popup-sections.initialized.v1";

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

type PopupSectionId = "status" | "channel" | "nicklist" | "query" | "custom" | "combined";

interface PopupSection {
  id: PopupSectionId;
  label: string;
  name: string;
  template: string;
}

export const POPUP_SECTIONS: PopupSection[] = [
  { id: "status", label: "Server / status", name: "popups-status", template: `; Server/status window right-click menu.
;   $style(1) puts a check mark on an item, $style(2) greys it out, $style(0)
;   leaves it plain — so $iif() can drive the state. It never shows in the menu.
menu status {
  Server info:echo -a Connected to $server ( $+ $network $+ ) as $me
  ; The tick tracks your away state.
  $style($iif($away,1,0)) Away:pops_away
  -
  ; One entry per channel you are on, built when the menu opens.
  Channels
  .$submenu($pops_chans($1))
  -
  Server links:links
  Reconnect:server
}

; jIRC calls this with "begin", then 1, 2, 3 ... then "end". Return one popup
; line each time ("label:command"), or nothing to end the run. "-" is a separator.
alias -l pops_chans {
  if ($1 == begin) return -
  if ($1 == end) return -
  var %c = $chan($1)
  if (%c == $null) return
  return %c $+ : $+ join %c
}

alias -l pops_away {
  if ($away) { away }
  else { away Back later }
}
` },
  { id: "channel", label: "Channel", name: "popups-channel", template: `; Channel window right-click menu.
;   Label:command  — everything after the FIRST colon is the command, so keep
;                    colons out of labels.
;   .  ..          — submenu depth, one dot per level.  -  is a separator.
menu channel {
  Topic:echo -a Topic for $chan is $chan($chan).topic
  Who is here:who $chan
  -
  ; Greys itself out unless you hold ops here.
  $style($iif($me isop $chan,0,2)) Channel modes
  .No external messages:mode $chan +n
  .Topic ops only:mode $chan +t
  .Invite only:mode $chan +i
  .Moderated:mode $chan +m
  .-
  .Remove moderation:mode $chan -m
  -
  Jump to
  .$submenu($popc_chans($1))
  -
  Clear this window:clear
  Part $chan:part $chan
}

alias -l popc_chans {
  if ($1 == begin) return -
  if ($1 == end) return -
  var %c = $chan($1)
  if (%c == $null) return
  return %c $+ : $+ join %c
}
` },
  { id: "nicklist", label: "Nick list", name: "popups-nicklist", template: `; Nick-list right-click menu.
;   $snick($active,1) is the first selected nick — using it in a label makes the
;   menu name the person you clicked. $snicks is every selected nick.
menu nicklist {
  Whois $snick($active,1):whois $snick($active,1)
  Query:query $snick($active,1)
  Slap:me slaps $snick($active,1) around a bit with a large trout
  -
  ; These grey out unless you hold ops.
  $style($iif($me isop $chan,0,2)) Give ops:mode $chan +o $snick($active,1)
  $style($iif($me isop $chan,0,2)) Take ops:mode $chan -o $snick($active,1)
  $style($iif($me isop $chan,0,2)) Voice:mode $chan +v $snick($active,1)
  $style($iif($me isop $chan,0,2)) Kick
  .Quietly:kick $chan $snick($active,1)
  .With a reason:kick $chan $snick($active,1) Please read the topic
  .Ban and kick:popn_bankick $chan $snick($active,1)
  -
  ; Greys out until you select two or more nicks.
  $style($iif($snick($active,2),0,2)) Selected $numtok($snicks,32) nicks
  .Whois each:popn_each whois $snicks
  .Query each:popn_each query $snicks
  .Copy to clipboard:clipboard $snicks
}

; Runs a command once per nick in a space-separated list.
alias -l popn_each {
  var %cmd = $1
  var %list = $2-
  var %i = 1
  while (%i <= $numtok(%list,32)) {
    %cmd $gettok(%list,%i,32)
    inc %i
  }
}

; Ban the host rather than the nick, so a rename does not dodge it.
alias -l popn_bankick {
  mode $1 +b $address($2,2)
  kick $1 $2
}
` },
  { id: "query", label: "Query", name: "popups-query", template: `; Private-query right-click menu.
menu query {
  Whois $target:whois $target
  Version check:ctcp $target VERSION
  -
  Close:close -m $target
}
` },
  { id: "custom", label: "Custom window", name: "popups-custom", template: `; Custom @window right-click menu.
; Change @mywindow to the exact name used by /window.
menu @mywindow {
  Clear:/clear
  Close:/window -c @mywindow
}
` },
  { id: "combined", label: "Combined / legacy", name: "popups", template: `; Optional combined popup file for imported or existing menu blocks.
; Dedicated context files are shown above entries from Remote scripts.
` },
];

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
  const [popupLoaded, setPopupLoaded] = useState<Record<string, boolean>>({});
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

  const selectPopup = async (popup: PopupSection, knownNames: string[] = names) => {
    const text = await api.scriptRead(popup.name).catch(() => "");
    const draft = localStorage.getItem(DRAFT_PREFIX + popup.name);
    const loaded = await api.scriptIsLoaded(popup.name).catch(() => true);
    setCurrent(popup.name);
    setSource(draft ?? (text || popup.template));
    setDirty(draft !== null || !knownNames.includes(popup.name));
    setPopupLoaded((state) => ({ ...state, [popup.name]: loaded }));
  };

  const refresh = async (selectName?: string, requestedSection: EditorSection = section) => {
    let list = await api.scriptsList().catch((): string[] => []);
    if (requestedSection === "popups" && localStorage.getItem(POPUP_INIT_KEY) !== "1") {
      for (const popup of POPUP_SECTIONS.filter((item) => item.id !== "combined")) {
        if (!list.includes(popup.name)) {
          await api.scriptWrite(popup.name, popup.template).catch(() => {});
        }
      }
      localStorage.setItem(POPUP_INIT_KEY, "1");
      list = await api.scriptsList().catch((): string[] => list);
    }
    setNames(list);
    if (requestedSection === "aliases") {
      const name = "aliases";
      const text = await api.scriptRead(name).catch(() => "");
      const draft = localStorage.getItem(DRAFT_PREFIX + name);
      setCurrent(name);
      setSource(draft ?? (text || ALIASES_EXAMPLE));
      setDirty(draft !== null || !list.includes(name));
      return;
    }
    if (requestedSection === "popups") {
      const states = await Promise.all(
        POPUP_SECTIONS.map(async (popup) => [popup.name, await api.scriptIsLoaded(popup.name).catch(() => true)] as const)
      );
      setPopupLoaded(Object.fromEntries(states));
      const selected = POPUP_SECTIONS.find((popup) => popup.name === current) ?? POPUP_SECTIONS[0];
      await selectPopup(selected, list);
      return;
    }
    const popupNames = new Set(POPUP_SECTIONS.map((popup) => popup.name));
    const remoteNames = list.filter((name) => name !== "aliases" && !popupNames.has(name));
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
    await refresh(undefined, section);
  };

  const togglePopup = async (name: string) => {
    if (section !== "popups") return;
    const loaded = !(popupLoaded[name] ?? true);
    await api.scriptSetLoaded(name, loaded).catch(() => {});
    setPopupLoaded((state) => ({ ...state, [name]: loaded }));
    setStatus(loaded ? "Popup section enabled" : "Popup section hidden");
    setTimeout(() => setStatus(""), 2000);
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
            <>Create menus by context. Dedicated popup files are displayed before menus from Remote scripts. Nested items begin with a dot; <code>-</code> adds a separator.</>
          ) : (
            <>Remote scripts live in <code>scripts/*.mrc</code>. All files are compiled together and can share aliases, events and variables.</>
          )}
        </p>
        <div className="script-layout">
          {section === "popups" && <div className="script-list popup-section-list">
            {POPUP_SECTIONS.map((popup) => (
              <div className="popup-section-row" key={popup.id}>
                <button
                  className={`script-list-item${popup.name === current ? " active" : ""}`}
                  onClick={() => void selectPopup(popup)}
                >
                  {popup.label}
                </button>
                <button
                  className={`popup-visibility${(popupLoaded[popup.name] ?? true) ? " enabled" : ""}`}
                  onClick={() => void togglePopup(popup.name)}
                  title={(popupLoaded[popup.name] ?? true) ? "Hide this popup section" : "Show this popup section"}
                  aria-label={(popupLoaded[popup.name] ?? true) ? `Disable ${popup.label} popup section` : `Enable ${popup.label} popup section`}
                >
                  {(popupLoaded[popup.name] ?? true) ? "On" : "Off"}
                </button>
              </div>
            ))}
          </div>}
          {section === "remote" && <div className="script-list">
            {names.filter((name) => name !== "aliases" && !POPUP_SECTIONS.some((popup) => popup.name === name)).map((n) => (
              <button
                key={n}
                className={`script-list-item${n === current ? " active" : ""}`}
                onClick={() => select(n)}
              >
                {n}
              </button>
            ))}
            {names.filter((name) => name !== "aliases" && !POPUP_SECTIONS.some((popup) => popup.name === name)).length === 0 && <div className="empty-hint">No remote scripts yet.</div>}
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
          {current && (section === "remote" || section === "popups") && (
            <button className="ghost danger-text" onClick={remove}>
              Delete {section === "popups" ? "section" : ""}
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
