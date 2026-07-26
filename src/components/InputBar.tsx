import { KeyboardEvent, MouseEvent, useRef, useState } from "react";
import { Buffer } from "../state/store";
import { handleInput } from "../lib/slash";
import { emojiPicker } from "../lib/emoji";
import { ContextMenu } from "./popupMenu";
import {
  applyPersistentColor,
  insertControl,
  IRC_FORMAT,
} from "../lib/inputFormatting";
import { useSettings } from "../state/settings";

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
  const [foreground, setForeground] = useState(DEFAULT_FOREGROUND);
  const [background, setBackground] = useState<number | undefined>();
  const [activeColours, setActiveColours] = useState<{
    foreground: number;
    background?: number;
  } | null>(null);
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);
  const showInputToolbar = useSettings((state) => state.showInputToolbar);
  const spellCheck = useSettings((state) => state.spellCheck);
  const spellCheckLanguage = useSettings((state) => state.spellCheckLanguage);
  const inputRef = useRef<HTMLInputElement>(null);
  const history = useRef<string[]>([]);
  const histIdx = useRef(-1);

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
    event.preventDefault();
    setPicker(false);
    setContextMenu({ x: event.clientX, y: event.clientY });
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
    const text = value;
    if (!text.trim()) return;
    history.current.push(text);
    histIdx.current = history.current.length;
    setValue("");
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
      completeNick();
    }
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
            <label className="composer-color-control">
              Text colour
              <select
                aria-label="Text colour"
                value={foreground}
                onChange={(event) => setForeground(Number(event.target.value))}
              >
                {IRC_COLORS.map((color, index) => (
                  <option key={index} value={index} style={{ backgroundColor: color }}>
                    {IRC_COLOR_NAMES[index]}
                  </option>
                ))}
              </select>
            </label>
            <label className="composer-color-control">
              Background
              <select
                aria-label="Background colour"
                value={background ?? ""}
                onChange={(event) =>
                  setBackground(event.target.value === "" ? undefined : Number(event.target.value))
                }
              >
                <option value="">None</option>
                {IRC_COLORS.map((color, index) => (
                  <option key={index} value={index} style={{ backgroundColor: color }}>
                    {IRC_COLOR_NAMES[index]}
                  </option>
                ))}
              </select>
            </label>
            <button
              type="button"
              title="Use these text and background colours until Reset is clicked"
              className={`input-color-apply${activeColours ? " active" : ""}`}
              style={{
                color: IRC_COLORS[foreground],
                backgroundColor: background === undefined ? undefined : IRC_COLORS[background],
              }}
              onClick={() => {
                setActiveColours({ foreground, background });
                inputRef.current?.focus();
              }}
            >
              {activeColours ? "Colours active" : "Apply colours"}
            </button>
            <button
              type="button"
              title="Restore default message colours"
              onClick={() => {
                setForeground(DEFAULT_FOREGROUND);
                setBackground(undefined);
                setActiveColours(null);
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
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={onKeyDown}
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
          <div className="menu-title">Message tools</div>
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
          <div className="menu-sep" />
          <div className="input-menu-palette">
            <span>Text colour</span>
            <div>
              {IRC_COLORS.map((color, index) => (
                <button
                  key={`fg-${index}`}
                  className={foreground === index ? "selected" : ""}
                  style={{ backgroundColor: color }}
                  title={IRC_COLOR_NAMES[index]}
                  aria-label={`${IRC_COLOR_NAMES[index]} text`}
                  onClick={() => setForeground(index)}
                />
              ))}
            </div>
          </div>
          <div className="input-menu-palette">
            <span>Background</span>
            <div>
              <button
                className={`no-colour${background === undefined ? " selected" : ""}`}
                title="No background"
                aria-label="No background"
                onClick={() => setBackground(undefined)}
              />
              {IRC_COLORS.map((color, index) => (
                <button
                  key={`bg-${index}`}
                  className={background === index ? "selected" : ""}
                  style={{ backgroundColor: color }}
                  title={IRC_COLOR_NAMES[index]}
                  aria-label={`${IRC_COLOR_NAMES[index]} background`}
                  onClick={() => setBackground(index)}
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
              Apply colours
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
              Reset colours
            </button>
          </div>
          <div className="menu-sep" />
          <button onClick={() => useSettings.getState().set("spellCheck", !spellCheck)}>
            <span className="pmenu-check">{spellCheck ? "✓" : ""}</span>
            Check spelling
          </button>
          <div className="menu-sep" />
          <button onClick={() => runEditCommand("undo")}>Undo</button>
          <button onClick={() => runEditCommand("cut")}>Cut</button>
          <button onClick={() => runEditCommand("copy")}>Copy</button>
          <button onClick={paste}>Paste</button>
          <button onClick={() => runEditCommand("selectAll")}>Select all</button>
        </ContextMenu>
      )}
    </div>
  );
}
