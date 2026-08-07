import { ChangeEvent, KeyboardEvent, MouseEvent, useEffect, useRef, useState } from "react";
import { Buffer } from "../state/store";
import { api } from "../lib/api";
import { handleInput } from "../lib/slash";
import { emojiPicker } from "../lib/emoji";
import { ContextMenu } from "./popupMenu";
import {
  applyPersistentColor,
  insertControl,
  IRC_FORMAT,
} from "../lib/inputFormatting";
import { useSettings } from "../state/settings";
import { sendTyping, typingLabel, typingSent, useTyping } from "../state/typing";
import {
  EDITBOX_COMMAND_EVENT,
  EditboxCommand,
} from "../lib/clientCommands";

const IRC_COLORS = [
  "#ffffff", "#000000", "#00007f", "#009300",
  "#ff0000", "#7f0000", "#9c009c", "#fc7f00",
  "#ffff00", "#00fc00", "#009393", "#00ffff",
  "#0000fc", "#ff00ff", "#7f7f7f", "#d2d2d2",
];
const IRC_COLOR_NAMES = [
  "White", "Black", "Navy", "Green",
  "Red", "Maroon", "Purple", "Orange",
  "Yellow", "Lime", "Teal", "Cyan",
  "Blue", "Pink", "Grey", "Light grey",
];
const DEFAULT_FOREGROUND = 1;

const COMMON_CORRECTIONS: Record<string, string> = {
  adn: "and",
  becuase: "because",
  definately: "definitely",
  dont: "don't",
  helo: "hello",
  heloo: "hello",
  helllo: "hello",
  hellp: "hello",
  hllo: "hello",
  hlelo: "hello",
  recieve: "receive",
  teh: "the",
  thier: "their",
  wierd: "weird",
  wont: "won't",
  wouldnt: "wouldn't",
};

export interface WordCorrection {
  start: number;
  end: number;
  original: string;
  replacement: string;
}

function matchCase(original: string, replacement: string): string {
  if (original === original.toUpperCase()) return replacement.toUpperCase();
  if (original[0] === original[0]?.toUpperCase()) {
    return replacement[0]?.toUpperCase() + replacement.slice(1);
  }
  return replacement;
}

export function correctionAt(value: string, caret: number): WordCorrection | null {
  const safeCaret = Math.max(0, Math.min(caret, value.length));
  let start = safeCaret;
  let end = safeCaret;
  while (start > 0 && /[A-Za-z']/.test(value[start - 1])) start -= 1;
  while (end < value.length && /[A-Za-z']/.test(value[end])) end += 1;
  const original = value.slice(start, end);
  const replacement = COMMON_CORRECTIONS[original.toLowerCase()];
  if (!original || !replacement) return null;
  return { start, end, original, replacement: matchCase(original, replacement) };
}

export function autoCorrectCompletedWord(value: string, caret: number) {
  const completedAt = Math.max(0, Math.min(caret - 1, value.length));
  const correction = correctionAt(value, completedAt);
  if (!correction || correction.end !== completedAt) return { value, caret };
  const next =
    value.slice(0, correction.start) +
    correction.replacement +
    value.slice(correction.end);
  return {
    value: next,
    caret: caret + correction.replacement.length - correction.original.length,
  };
}

export function spellCheckAttributes(enabled: boolean, language: string) {
  return {
    spellCheck: enabled,
    lang: language || undefined,
  };
}

export function replaceInputSelection(
  value: string,
  start: number,
  end: number,
  replacement: string
) {
  return {
    value: value.slice(0, start) + replacement + value.slice(end),
    caret: start + replacement.length,
  };
}

export function InputBar({ buffer }: { buffer: Buffer }) {
  const [value, setValue] = useState("");
  const [picker, setPicker] = useState(false);
  const [colorPicker, setColorPicker] = useState(false);
  const [foreground, setForeground] = useState(DEFAULT_FOREGROUND);
  const [background, setBackground] = useState<number | undefined>();
  const [activeColours, setActiveColours] = useState<{
    foreground: number;
    background?: number;
  } | null>(null);
  const [contextMenu, setContextMenu] = useState<{
    x: number;
    y: number;
    correction: WordCorrection | null;
  } | null>(null);
  const [colourMenu, setColourMenu] = useState<"foreground" | "background" | null>(null);
  const showInputToolbar = useSettings((state) => state.showInputToolbar);
  const spellCheck = useSettings((state) => state.spellCheck);
  const autoCorrect = useSettings((state) => state.autoCorrect);
  const spellCheckLanguage = useSettings((state) => state.spellCheckLanguage);
  const inputRef = useRef<HTMLInputElement>(null);
  const history = useRef<string[]>([]);
  const histIdx = useRef(-1);

  const reportEditbox = () => {
    const input = inputRef.current;
    void api.scriptSetClientEditbox(
      buffer.name,
      input?.value ?? value,
      input?.selectionStart ?? value.length,
      input?.selectionEnd ?? value.length
    ).catch(() => {});
  };

  useEffect(() => {
    reportEditbox();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [buffer.name, value]);

  useEffect(() => {
    const listener = (event: Event) => {
      const command = (event as CustomEvent<EditboxCommand>).detail;
      if (
        command.serverId !== buffer.serverId ||
        command.target.toLowerCase() !== buffer.name.toLowerCase()
      ) {
        return;
      }
      const next = command.text + (command.appendSpace ? " " : "");
      setValue(command.submit ? "" : next);
      if (command.submit && next.trim()) {
        history.current.push(next);
        histIdx.current = history.current.length;
        void handleInput(next, buffer);
      }
      requestAnimationFrame(() => {
        const input = inputRef.current;
        if (!input) return;
        const start = Math.min(command.selectionStart ?? next.length, next.length);
        const end = Math.min(command.selectionEnd ?? start, next.length);
        input.setSelectionRange(start, end);
        if (command.focus || command.submit) input.focus();
      });
    };
    window.addEventListener(EDITBOX_COMMAND_EVENT, listener);
    return () => window.removeEventListener(EDITBOX_COMMAND_EVENT, listener);
  }, [buffer.name, buffer.serverId]);

  const insertEmoji = (s: string) => {
    const input = inputRef.current;
    const start = input?.selectionStart ?? value.length;
    const end = input?.selectionEnd ?? start;
    const edit = replaceInputSelection(value, start, end, s);
    setValue(edit.value);
    setPicker(false);
    requestAnimationFrame(() => {
      inputRef.current?.focus();
      inputRef.current?.setSelectionRange(edit.caret, edit.caret);
    });
  };

  const openContextMenu = (event: MouseEvent<HTMLInputElement>) => {
    // Shift+right-click leaves the native WebView menu available for full
    // operating-system dictionary suggestions.
    if (event.shiftKey && spellCheck) return;
    event.preventDefault();
    setPicker(false);
    setColourMenu(null);
    const caret = event.currentTarget.selectionStart ?? value.length;
    setContextMenu({
      x: event.clientX,
      y: event.clientY,
      correction: correctionAt(value, caret),
    });
  };

  const replaceCorrection = (correction: WordCorrection) => {
    const edit = replaceInputSelection(
      value,
      correction.start,
      correction.end,
      correction.replacement
    );
    setValue(edit.value);
    setContextMenu(null);
    requestAnimationFrame(() => {
      inputRef.current?.focus();
      inputRef.current?.setSelectionRange(edit.caret, edit.caret);
    });
  };

  /** Whether `+typing` applies here: a channel or a query, never the status
   *  window or a script-owned @window. */
  const conversational = buffer.kind === "channel" || buffer.kind === "query";

  const noteTyping = (text: string) => {
    if (!conversational) return;
    // A leading `/` is a command, not conversation — announcing it would
    // leak that you are about to run something.
    sendTyping(buffer.serverId, buffer.name, text.trim().length > 0 && !text.startsWith("/"));
  };

  const onInputChange = (event: ChangeEvent<HTMLInputElement>) => {
    const next = event.target.value;
    noteTyping(next);
    const caret = event.target.selectionStart ?? next.length;
    const typed = next[caret - 1] ?? "";
    if (autoCorrect && /[\s.,!?;:]/.test(typed)) {
      const corrected = autoCorrectCompletedWord(next, caret);
      setValue(corrected.value);
      if (corrected.value !== next) {
        requestAnimationFrame(() =>
          inputRef.current?.setSelectionRange(corrected.caret, corrected.caret)
        );
      }
      return;
    }
    setValue(next);
  };

  const runEditCommand = (command: "undo" | "cut" | "copy" | "selectAll") => {
    inputRef.current?.focus();
    document.execCommand(command);
    setContextMenu(null);
  };

  const paste = async () => {
    try {
      const text = await navigator.clipboard.readText();
      const input = inputRef.current;
      const start = input?.selectionStart ?? value.length;
      const end = input?.selectionEnd ?? start;
      const edit = replaceInputSelection(value, start, end, text);
      setValue(edit.value);
      requestAnimationFrame(() => {
        inputRef.current?.focus();
        inputRef.current?.setSelectionRange(edit.caret, edit.caret);
      });
    } catch {
      inputRef.current?.focus();
      document.execCommand("paste");
    }
    setContextMenu(null);
  };

  const applyControl = (control: string, close = control) => {
    const input = inputRef.current;
    const start = input?.selectionStart ?? value.length;
    const end = input?.selectionEnd ?? start;
    const edit = insertControl(value, start, end, control, close);
    setValue(edit.value);
    requestAnimationFrame(() => {
      inputRef.current?.focus();
      inputRef.current?.setSelectionRange(edit.selectionStart, edit.selectionEnd);
    });
  };

  const submit = async () => {
    const finalCorrection = correctionAt(value, value.length);
    const text =
      autoCorrect && finalCorrection?.end === value.length
        ? replaceInputSelection(
            value,
            finalCorrection.start,
            finalCorrection.end,
            finalCorrection.replacement
          ).value
        : value;
    if (!text.trim()) return;
    history.current.push(text);
    histIdx.current = history.current.length;
    setValue("");
    if (conversational) typingSent(buffer.serverId, buffer.name);
    await handleInput(
      applyPersistentColor(
        text,
        activeColours !== null,
        activeColours?.foreground ?? foreground,
        activeColours?.background
      ),
      buffer
    );
  };

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    const modifiers = [e.ctrlKey && "ctrl", e.altKey && "alt", e.shiftKey && "shift", e.metaKey && "meta"].filter(Boolean).join("+");
    void api.scriptRunKey(buffer.serverId, buffer.name, "KEYDOWN", e.key, e.keyCode, e.repeat, modifiers, value);
    if (buffer.name.startsWith("@") && Array.from(e.key).length === 1) {
      void api.scriptRunKey(buffer.serverId, buffer.name, "CHAR", e.key, e.key.codePointAt(0) ?? 0, e.repeat, modifiers, value);
    }
    if (e.key === "Enter") {
      e.preventDefault();
      submit();
    } else if (e.key === "ArrowUp") {
      if (histIdx.current > 0) {
        histIdx.current -= 1;
        setValue(history.current[histIdx.current] ?? "");
      }
      e.preventDefault();
    } else if (e.key === "ArrowDown") {
      if (histIdx.current < history.current.length - 1) {
        histIdx.current += 1;
        setValue(history.current[histIdx.current] ?? "");
      } else {
        histIdx.current = history.current.length;
        setValue("");
      }
      e.preventDefault();
    } else if (e.key === "Tab") {
      e.preventDefault();
      void api.scriptRunTabcomp(buffer.serverId, buffer.name, value)
        .then((halted) => { if (!halted) completeNick(); })
        .catch(() => completeNick());
    }
  };

  const onKeyUp = (e: KeyboardEvent<HTMLInputElement>) => {
    const modifiers = [e.ctrlKey && "ctrl", e.altKey && "alt", e.shiftKey && "shift", e.metaKey && "meta"].filter(Boolean).join("+");
    void api.scriptRunKey(buffer.serverId, buffer.name, "KEYUP", e.key, e.keyCode, e.repeat, modifiers, value);
  };

  // Simple nick tab-completion from the last word.
  const completeNick = () => {
    const words = value.split(" ");
    const partial = words[words.length - 1].toLowerCase();
    if (!partial) return;
    const match = buffer.members.find((m) => m.nick.toLowerCase().startsWith(partial));
    if (match) {
      words[words.length - 1] = words.length === 1 ? `${match.nick}:` : match.nick;
      setValue(words.join(" ") + " ");
    }
  };

  return (
    <div className="inputbar">
      <TypingIndicator buffer={buffer} />
      {picker && (
        <>
          <div className="emoji-backdrop" onClick={() => setPicker(false)} />
          <div className="emoji-picker">
            {emojiPicker().map((e, i) => (
              <button key={i} title={e.title} onClick={() => insertEmoji(e.insert)}>
                {e.img ? <img src={e.img} alt={e.title} /> : e.glyph}
              </button>
            ))}
          </div>
        </>
      )}
      {colorPicker && (
        <>
          <div className="emoji-backdrop" onClick={() => setColorPicker(false)} />
          {/* A 16-cell grid rather than two <select>s: WebView2 ignores
              background-color on <option>, so a dropdown could only ever list
              colour *names*. The grid shows the actual colour everywhere. */}
          <div className="color-picker" role="dialog" aria-label="Message colours">
            <div className="color-picker-group">
              <div className="color-picker-title" id="fg-colours">
                Text colour
              </div>
              <div className="color-grid" role="radiogroup" aria-labelledby="fg-colours">
                {IRC_COLORS.map((color, index) => (
                  <button
                    key={index}
                    type="button"
                    role="radio"
                    aria-checked={foreground === index}
                    className={`color-cell${foreground === index ? " selected" : ""}`}
                    style={{ backgroundColor: color }}
                    title={IRC_COLOR_NAMES[index]}
                    aria-label={IRC_COLOR_NAMES[index]}
                    onClick={() => setForeground(index)}
                  />
                ))}
              </div>
            </div>
            <div className="color-picker-group">
              <div className="color-picker-title" id="bg-colours">
                Background
              </div>
              <div className="color-grid with-none" role="radiogroup" aria-labelledby="bg-colours">
                <button
                  type="button"
                  role="radio"
                  aria-checked={background === undefined}
                  className={`color-cell none${background === undefined ? " selected" : ""}`}
                  title="No background"
                  aria-label="No background"
                  onClick={() => setBackground(undefined)}
                />
                {IRC_COLORS.map((color, index) => (
                  <button
                    key={index}
                    type="button"
                    role="radio"
                    aria-checked={background === index}
                    className={`color-cell${background === index ? " selected" : ""}`}
                    style={{ backgroundColor: color }}
                    title={IRC_COLOR_NAMES[index]}
                    aria-label={IRC_COLOR_NAMES[index]}
                    onClick={() => setBackground(index)}
                  />
                ))}
              </div>
            </div>
          </div>
        </>
      )}
      <div className="composer-toolbar" role="toolbar" aria-label="Message tools">
        <button
          type="button"
          className="emoji-btn"
          title="Choose an emoji"
          onClick={() => setPicker((p) => !p)}
        >
          <span aria-hidden="true">😀</span>
        </button>
        {showInputToolbar && (
          <>
            <span className="composer-divider" />
            <div className="input-format-buttons" aria-label="Text style">
              <button type="button" title="Bold" aria-label="Bold" onClick={() => applyControl(IRC_FORMAT.bold)}>
                <strong>B</strong>
              </button>
              <button type="button" title="Italic" aria-label="Italic" onClick={() => applyControl(IRC_FORMAT.italic)}>
                <em>I</em>
              </button>
              <button type="button" title="Underline" aria-label="Underline" onClick={() => applyControl(IRC_FORMAT.underline)}>
                <u>U</u>
              </button>
            </div>
            {/* The swatch previews the chosen colours; the button applies them.
                Keeping those apart is what lets the button keep the theme's own
                contrast instead of painting itself in an arbitrary IRC colour. */}
            <button
              type="button"
              className={`color-swatch${activeColours ? " active" : ""}`}
              aria-label={
                activeColours
                  ? `Colours active: ${IRC_COLOR_NAMES[activeColours.foreground]} on ${
                      activeColours.background === undefined
                        ? "no background"
                        : IRC_COLOR_NAMES[activeColours.background]
                    }. Choose colours`
                  : "Choose text and background colours"
              }
              aria-expanded={colorPicker}
              title={
                activeColours ? "Colours are active — click to change" : "Choose message colours"
              }
              onClick={() => setColorPicker((open) => !open)}
            >
              <span
                className="color-swatch-chip"
                style={{
                  color: IRC_COLORS[foreground],
                  // With no background the message sits on the chat background,
                  // so preview it there — that is what the reader will see.
                  backgroundColor:
                    background === undefined ? "var(--bg)" : IRC_COLORS[background],
                }}
                aria-hidden="true"
              >
                Aa
              </span>
              <span className="color-swatch-label">
                {IRC_COLOR_NAMES[foreground]}
                {background === undefined ? "" : ` on ${IRC_COLOR_NAMES[background]}`}
              </span>
            </button>
            <button
              type="button"
              title="Use these text and background colours until Reset is clicked"
              className="input-color-apply"
              onClick={() => {
                setActiveColours({ foreground, background });
                inputRef.current?.focus();
              }}
            >
              Apply
            </button>
            <button
              type="button"
              title="Restore default message colours"
              onClick={() => {
                setForeground(DEFAULT_FOREGROUND);
                setBackground(undefined);
                setActiveColours(null);
                setColorPicker(false);
                inputRef.current?.focus();
              }}
            >
              Reset
            </button>
          </>
        )}
      </div>
      <div className="composer-input-row">
        <input
          ref={inputRef}
          value={value}
          placeholder="Type a message or /command…"
          onChange={onInputChange}
          onSelect={reportEditbox}
          onKeyDown={onKeyDown}
          onKeyUp={onKeyUp}
          onContextMenu={openContextMenu}
          {...spellCheckAttributes(spellCheck, spellCheckLanguage)}
          autoFocus
        />
      </div>
      {contextMenu && (
        <ContextMenu
          x={contextMenu.x}
          y={contextMenu.y}
          onClose={() => setContextMenu(null)}
        >
          {contextMenu.correction && (
            <>
              <button
                className="spelling-suggestion"
                onClick={() => replaceCorrection(contextMenu.correction!)}
              >
                <span className="pmenu-check">✓</span>
                {contextMenu.correction.replacement}
              </button>
              <div className="menu-sep" />
            </>
          )}
          <button onClick={() => runEditCommand("undo")}>Undo</button>
          <button onClick={() => runEditCommand("cut")}>Cut</button>
          <button onClick={() => runEditCommand("copy")}>Copy</button>
          <button onClick={paste}>Paste</button>
          <button onClick={() => runEditCommand("selectAll")}>Select all</button>
          <div className="menu-sep" />
          <button
            onClick={() => {
              setContextMenu(null);
              setPicker(true);
            }}
          >
            <span>😀 Emoji</span>
          </button>
          <div className="input-menu-format">
            <button onClick={() => { applyControl(IRC_FORMAT.bold); setContextMenu(null); }}>
              <strong>B</strong> Bold
            </button>
            <button onClick={() => { applyControl(IRC_FORMAT.italic); setContextMenu(null); }}>
              <em>I</em> Italic
            </button>
            <button onClick={() => { applyControl(IRC_FORMAT.underline); setContextMenu(null); }}>
              <u>U</u> Underline
            </button>
          </div>
          <div className="input-menu-actions colour-choices">
            <button
              onClick={() => setColourMenu((open) => open === "foreground" ? null : "foreground")}
            >
              Text colour…
            </button>
            <button
              onClick={() => setColourMenu((open) => open === "background" ? null : "background")}
            >
              Background…
            </button>
          </div>
          {colourMenu && (
            <>
              <div className="input-menu-palette compact">
                <span>{colourMenu === "foreground" ? "Text colour" : "Background"}</span>
                <div>
                  {colourMenu === "background" && (
                    <button
                      className={`no-colour${background === undefined ? " selected" : ""}`}
                      title="No background"
                      aria-label="No background"
                      onClick={() => setBackground(undefined)}
                    />
                  )}
                  {IRC_COLORS.map((color, index) => (
                    <button
                      key={`${colourMenu}-${index}`}
                      className={
                        (colourMenu === "foreground" ? foreground === index : background === index)
                          ? "selected"
                          : ""
                      }
                      style={{ backgroundColor: color }}
                      title={IRC_COLOR_NAMES[index]}
                      aria-label={`${IRC_COLOR_NAMES[index]} ${colourMenu === "foreground" ? "text" : "background"}`}
                      onClick={() =>
                        colourMenu === "foreground"
                          ? setForeground(index)
                          : setBackground(index)
                      }
                    />
                  ))}
                </div>
              </div>
              <div className="input-menu-actions">
                <button
                  onClick={() => {
                    setActiveColours({ foreground, background });
                    setContextMenu(null);
                    inputRef.current?.focus();
                  }}
                >
                  Apply
                </button>
                <button
                  onClick={() => {
                    setForeground(DEFAULT_FOREGROUND);
                    setBackground(undefined);
                    setActiveColours(null);
                    setContextMenu(null);
                    inputRef.current?.focus();
                  }}
                >
                  Reset
                </button>
              </div>
            </>
          )}
          <div className="menu-sep" />
          <button onClick={() => useSettings.getState().set("spellCheck", !spellCheck)}>
            <span className="pmenu-check">{spellCheck ? "✓" : ""}</span>
            Check spelling
          </button>
          <button onClick={() => useSettings.getState().set("autoCorrect", !autoCorrect)}>
            <span className="pmenu-check">{autoCorrect ? "✓" : ""}</span>
            Auto-correct common typos
          </button>
          {spellCheck && (
            <div className="menu-note">Shift+right-click for system suggestions</div>
          )}
        </ContextMenu>
      )}
    </div>
  );
}

/** "<nick> is typing..." above the editor, from IRCv3 `+typing` notifications.
 *
 *  Renders nothing at all when nobody is typing - the row must not reserve
 *  height, or the input would jump every time someone starts and stops. */
function TypingIndicator({ buffer }: { buffer: Buffer }) {
  const names = useTyping(
    (s) => s.byBuffer[buffer.key]?.map((t) => t.nick).join(" ") ?? ""
  );
  if (!names) return null;
  return (
    <div className="typing-indicator" aria-live="polite">
      <span className="typing-dots" aria-hidden="true">
        <i />
        <i />
        <i />
      </span>
      {typingLabel(names.split(" "))}
    </div>
  );
}
