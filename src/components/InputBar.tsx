import { KeyboardEvent, useRef, useState } from "react";
import { Buffer } from "../state/store";
import { handleInput } from "../lib/slash";
import { emojiPicker } from "../lib/emoji";
import {
  colorControl,
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

export function InputBar({ buffer }: { buffer: Buffer }) {
  const [value, setValue] = useState("");
  const [picker, setPicker] = useState(false);
  const [foreground, setForeground] = useState(1);
  const [background, setBackground] = useState<number | undefined>();
  const showInputToolbar = useSettings((state) => state.showInputToolbar);
  const inputRef = useRef<HTMLInputElement>(null);
  const history = useRef<string[]>([]);
  const histIdx = useRef(-1);

  const insertEmoji = (s: string) => {
    setValue((v) => (v && !v.endsWith(" ") ? `${v} ${s} ` : `${v}${s} `));
    setPicker(false);
    inputRef.current?.focus();
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
    await handleInput(text, buffer);
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
          <span aria-hidden="true">😀</span> Emoji
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
              <span className="composer-color-swatch" style={{ backgroundColor: IRC_COLORS[foreground] }} />
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
              <span
                className={`composer-color-swatch${background === undefined ? " none" : ""}`}
                style={background === undefined ? undefined : { backgroundColor: IRC_COLORS[background] }}
              />
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
              title="Apply the selected text and background colours"
              className="input-color-apply"
              style={{
                color: IRC_COLORS[foreground],
                backgroundColor: background === undefined ? undefined : IRC_COLORS[background],
              }}
              onClick={() => applyControl(colorControl(foreground, background), IRC_FORMAT.reset)}
            >
              Apply colours
            </button>
            <button type="button" title="Clear all formatting from this point" onClick={() => applyControl(IRC_FORMAT.reset, "")}>
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
          autoFocus
        />
      </div>
    </div>
  );
}
