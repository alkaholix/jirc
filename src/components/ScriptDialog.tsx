import { useEffect, useMemo, useState } from "react";
import { api } from "../lib/api";
import { MslEditor } from "./MslEditor";
import { mslDiagnostics } from "../lib/mslLanguage";
import { SCRIPT_THEMES } from "../lib/mslLanguage";
import { useSettings, type ScriptTheme } from "../state/settings";

const DRAFT_PREFIX = "jirc.script-draft.";
/// Text the draft was taken from. A draft is only unsaved work relative to a
/// particular version of the file; once the file changes underneath it (an
/// upgrade reseeding a default, or an edit from elsewhere) the draft is stale.
const DRAFT_BASE_PREFIX = "jirc.script-draft-base.";
const POPUP_INIT_KEY = "jirc.popup-sections.initialized.v1";
/// Drafts written before base tracking existed cannot be checked for staleness,
/// and were silently shadowing reseeded defaults with no way to discard them.
/// Clear them once, then never again.
const DRAFT_MIGRATION_KEY = "jirc.script-drafts.migrated.v1";

export function clearDraft(name: string) {
  localStorage.removeItem(DRAFT_PREFIX + name);
  localStorage.removeItem(DRAFT_BASE_PREFIX + name);
}

export function saveDraft(name: string, value: string, base: string) {
  localStorage.setItem(DRAFT_PREFIX + name, value);
  localStorage.setItem(DRAFT_BASE_PREFIX + name, base);
}

/// Returns the draft to show for `name`, or null when there is none worth
/// keeping. A draft identical to the file, or taken from a different version of
/// it, is dropped rather than shown — otherwise it hides the real file forever.
export function liveDraft(name: string, fileText: string): string | null {
  const draft = localStorage.getItem(DRAFT_PREFIX + name);
  if (draft === null) return null;
  if (draft === fileText) {
    clearDraft(name);
    return null;
  }
  const base = localStorage.getItem(DRAFT_BASE_PREFIX + name);
  if (base !== null && base !== fileText) {
    clearDraft(name);
    return null;
  }
  return draft;
}

/// One-time cleanup of pre-base-tracking drafts.
export function migrateLegacyDrafts() {
  if (localStorage.getItem(DRAFT_MIGRATION_KEY) === "1") return;
  for (const key of Object.keys(localStorage)) {
    if (key.startsWith(DRAFT_PREFIX) && !localStorage.getItem(DRAFT_BASE_PREFIX + key.slice(DRAFT_PREFIX.length))) {
      localStorage.removeItem(key);
    }
  }
  localStorage.setItem(DRAFT_MIGRATION_KEY, "1");
}

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
  { id: "status", label: "Server / status", name: "popups-status", template: `; Server / status window right-click menu.
;
; $style(1) puts a tick on an item, $style(2) greys it out, $style(0) leaves it
; plain — so $iif() can drive the state from live values. It never displays.

menu status {
  ; Information lines, greyed so they read as status rather than actions.
  $style(2) $me on $network
  $style(2) Connected for $duration($uptime(server,3))
  -
  ; The tick tracks your away state, and the item toggles it.
  $style($iif($away,1,0)) $iif($away,Back ( $+ $duration($awaytime) $+ ),Set away):pops_away
  -
  ; One entry per channel, each showing its user count.
  Channels ( $+ $chan(0) $+ )
  .$submenu($pops_chans($1))
  -
  Server
  .Links:links
  .Message of the day:motd
  .Server time:time
  .Admin:admin
  -
  Copy
  .Server name:clipboard $server
  .My address:clipboard $address($me,5)
  -
  Reconnect:server
}

; --- helpers (alias -l keeps them private to this file) ---

; $submenu calls this with "begin", then 1, 2, 3 ... then "end". Return one
; "label:command" line each time, "-" for a separator, or nothing to stop.
alias -l pops_chans {
  if ($1 == begin) return -
  if ($1 == end) return
  var %c = $chan($1)
  if (%c == $null) return
  return %c ( $+ $nick(%c,0) $+ ) $+ : $+ join %c
}

alias -l pops_away {
  if ($away) { away }
  else { away Back later }
}
` },
  { id: "channel", label: "Channel", name: "popups-channel", template: `; Channel window right-click menu.
;
;   Label:command   everything after the FIRST colon is the command, so keep
;                   colons out of labels
;   .  ..           submenu depth, one dot per level.   -   is a separator
;   $style(1)       tick   $style(2)  greyed   $style(0)  plain
;
; The mode items read the channel's live mode string, so their ticks always
; reflect what is actually set, and they toggle rather than only turning on.

menu channel {
  ; A live summary line — not clickable, just information.
  $style(2) $chan has $nick($chan,0) users
  Topic:echo -a Topic for $chan is $chan($chan).topic
  Who is here:who $chan
  -
  $style($iif($me isop $chan,0,2)) Modes
  .$style($iif(n isincs $chan($chan).mode,1,0)) No external messages:mode $chan $iif(n isincs $chan($chan).mode,-n,+n)
  .$style($iif(t isincs $chan($chan).mode,1,0)) Topic ops only:mode $chan $iif(t isincs $chan($chan).mode,-t,+t)
  .$style($iif(m isincs $chan($chan).mode,1,0)) Moderated:mode $chan $iif(m isincs $chan($chan).mode,-m,+m)
  .$style($iif(i isincs $chan($chan).mode,1,0)) Invite only:mode $chan $iif(i isincs $chan($chan).mode,-i,+i)
  .$style($iif(s isincs $chan($chan).mode,1,0)) Secret:mode $chan $iif(s isincs $chan($chan).mode,-s,+s)
  ; The current ban list, pulled from live channel state. Clicking one lifts it.
  $style($iif($me isop $chan,0,2)) Bans ( $+ $banlist($chan,0) $+ )
  .$submenu($popc_bans($1))
  -
  ; One entry per channel you are on, each showing its user count.
  Jump to
  .$submenu($popc_chans($1))
  -
  Copy
  .Channel name:clipboard $chan
  .Topic:clipboard $chan($chan).topic
  .User list:clipboard $popc_nicks
  -
  Clear this window:clear
  Part $chan:part $chan
}

; --- helpers (alias -l keeps them private to this file) ---

; Lists the channel's active bans. $banlist(#chan,0) is the count, and
; $banlist(#chan,N) the Nth mask.
alias -l popc_bans {
  if ($1 == begin) return -
  if ($1 == end) return
  var %b = $banlist($chan,$1)
  if (%b == $null) return
  return Unban %b $+ : $+ mode $chan -b %b
}

; One item per joined channel, labelled with its user count.
alias -l popc_chans {
  if ($1 == begin) return -
  if ($1 == end) return
  var %c = $chan($1)
  if (%c == $null) return
  return %c ( $+ $nick(%c,0) $+ ) $+ : $+ join %c
}

; Every nick in the channel as one space-separated line.
alias -l popc_nicks {
  var %out
  var %i = 1
  while (%i <= $nick($chan,0)) {
    var %out = %out $nick($chan,%i)
    inc %i
  }
  ; $gettok with a 1- range re-joins the tokens, dropping the leading space.
  return $gettok(%out,1-,32)
}
` },
  { id: "nicklist", label: "Nick list", name: "popups-nicklist", template: `; Nick-list right-click menu.
;
; Techniques on show here:
;   $snick($active,1)  the first selected nick   ·   $snicks = all of them
;   $style(1|2|3)      tick / greyed / both — must be first, never displayed
;   $submenu($x($1))   builds a whole run of items at runtime
;   $iif(...)          labels and states that follow live channel state

menu nicklist {
  ; The tick appears when this person already holds the mode, so the menu
  ; doubles as a status readout.
  Whois $snick($active,1):whois $snick($active,1)
  Query:query $snick($active,1)
  -
  $style($iif($me isop $chan,0,2)) $iif($snick($active,1) isop $chan,Take ops,Give ops):mode $chan $iif($snick($active,1) isop $chan,-o,+o) $snick($active,1)
  $style($iif($me isop $chan,0,2)) $iif($snick($active,1) isvoice $chan,Take voice,Give voice):mode $chan $iif($snick($active,1) isvoice $chan,-v,+v) $snick($active,1)
  $style($iif($me isop $chan,0,2)) Kick
  .Quietly:kick $chan $snick($active,1)
  .With a reason:kick $chan $snick($active,1) Please read the topic
  .Kick and ban the host:popn_bankick $chan $snick($active,1)
  -
  ; Every mIRC ban-mask type, built from this user's real address so you can
  ; see exactly what each one would match before you use it.
  $style($iif($me isop $chan,0,2)) Ban with mask
  .$submenu($popn_masks($1))
  ; Channels you share with them, discovered from the internal address list.
  Also on
  .$submenu($popn_common($1))
  -
  Send a CTCP
  .Version:ctcp $snick($active,1) VERSION
  .Ping:ctcp $snick($active,1) PING
  .Time:ctcp $snick($active,1) TIME
  .Client info:ctcp $snick($active,1) CLIENTINFO
  -
  Copy
  .Nick:clipboard $snick($active,1)
  .Address:clipboard $address($snick($active,1),5)
  -
  ; Greys out until two or more nicks are selected, then acts on all of them.
  $style($iif($snick($active,2),0,2)) Selected $numtok($snicks,32) nicks
  .Whois each:popn_each whois $snicks
  .Query each:popn_each query $snicks
  .Voice all:popn_mode $chan +v $snicks
  .Devoice all:popn_mode $chan -v $snicks
  .Copy nicks:clipboard $snicks
}

; --- helpers (alias -l keeps them private to this file) ---

; $submenu calls this with "begin", then 1, 2, 3 ... then "end". Return one
; "label:command" line each time, "-" for a separator, or nothing to stop.
; Here each entry previews a different $mask type against the real address.
alias -l popn_masks {
  if ($1 == begin) return -
  if ($1 == end) return
  var %n = $calc($1 - 1)
  if (%n > 9) return
  var %addr = $address($snick($active,1),5)
  if (%addr == $null) return
  var %m = $mask(%addr,%n)
  return %n $+ $chr(32) $+ %m $+ : $+ mode $chan +b %m
}

; Channels you and this user are both on.
alias -l popn_common {
  if ($1 == begin) return -
  if ($1 == end) return
  var %c = $comchan($snick($active,1),$1)
  if (%c == $null) return
  return %c $+ : $+ join %c
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

; Applies one mode to every selected nick, four at a time like a real client.
alias -l popn_mode {
  var %chan = $1
  var %flag = $2
  var %list = $3-
  var %i = 1
  while (%i <= $numtok(%list,32)) {
    mode %chan %flag $gettok(%list,%i,32)
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
;
; $target is the person you are talking to. The address items come from the
; internal address list, so they are populated once you have seen them speak.

menu query {
  $style(2) Talking to $target
  Whois $target:whois $target
  -
  Send a CTCP
  .Version:ctcp $target VERSION
  .Ping:ctcp $target PING
  .Time:ctcp $target TIME
  .Client info:ctcp $target CLIENTINFO
  -
  ; Channels you share with them.
  Also on
  .$submenu($popq_common($1))
  -
  Copy
  .Nick:clipboard $target
  .Address:clipboard $address($target,5)
  .Host mask:clipboard $mask($address($target,5),2)
  -
  Clear this window:clear
  Close:close -m $target
}

; --- helpers (alias -l keeps them private to this file) ---

; $submenu calls this with "begin", then 1, 2, 3 ... then "end". Return one
; "label:command" line each time, or nothing to stop.
alias -l popq_common {
  if ($1 == begin) return -
  if ($1 == end) return
  var %c = $comchan($target,$1)
  if (%c == $null) return
  return %c $+ : $+ join %c
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
  // The text currently on disk for `current`. A draft is unsaved work relative
  // to this, so it is what we record as the draft's base.
  const [fileText, setFileText] = useState("");
  const scriptTheme = useSettings((state) => state.scriptTheme);
  const setSetting = useSettings((state) => state.set);
  const diagnostics = useMemo(() => mslDiagnostics(source), [source]);

  const select = async (name: string) => {
    const text = await api.scriptRead(name).catch(() => "");
    const draft = liveDraft(name, text);
    setCurrent(name);
    setFileText(text);
    setSource(draft ?? text);
    setDirty(draft !== null);
  };

  const selectPopup = async (popup: PopupSection, knownNames: string[] = names) => {
    const text = await api.scriptRead(popup.name).catch(() => "");
    const draft = liveDraft(popup.name, text);
    const loaded = await api.scriptIsLoaded(popup.name).catch(() => true);
    setCurrent(popup.name);
    setFileText(text);
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
      const draft = liveDraft(name, text);
      setCurrent(name);
      setFileText(text);
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
    migrateLegacyDrafts();
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
      clearDraft(current);
      setFileText(source);
      setDirty(false);
      setStatus("Saved & compiled ✓");
      setTimeout(() => setStatus(""), 2000);
    } catch (e) {
      setStatus(`Error: ${e}`);
    }
  };

  // Throws away the cached draft and reloads from disk. Without this a draft
  // can only be cleared by saving or deleting the script.
  const discardDraft = async () => {
    if (!current) return;
    clearDraft(current);
    const popup = POPUP_SECTIONS.find((item) => item.name === current);
    if (popup) await selectPopup(popup);
    else await select(current);
    setStatus("Reloaded from disk");
    setTimeout(() => setStatus(""), 2000);
  };

  const remove = async () => {
    if (!current) return;
    if (!confirm(`Delete script "${current}"?`)) return;
    await api.scriptDelete(current).catch(() => {});
    clearDraft(current);
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
                if (current && dirty) saveDraft(current, source, fileText);
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
                  saveDraft(current, value, fileText);
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
          <button
            className="ghost"
            onClick={discardDraft}
            disabled={!current || !dirty}
            title="Throw away unsaved changes and reload this file from disk"
          >
            Discard changes
          </button>
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
