import { KeyboardEvent, useRef, useState } from "react";
import { Buffer } from "../state/store";
import { handleInput } from "../lib/slash";
import { emojiPicker } from "../lib/emoji";

export function InputBar({ buffer }: { buffer: Buffer }) {
  const [value, setValue] = useState("");
  const [picker, setPicker] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const history = useRef<string[]>([]);
  const histIdx = useRef(-1);

  const insertEmoji = (s: string) => {
    setValue((v) => (v && !v.endsWith(" ") ? `${v} ${s} ` : `${v}${s} `));
    setPicker(false);
    inputRef.current?.focus();
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
      <button
        className="emoji-btn"
        title="Emoji"
        onClick={() => setPicker((p) => !p)}
      >
        😀
      </button>
      <input
        ref={inputRef}
        value={value}
        placeholder="Type a message or /command…"
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={onKeyDown}
        autoFocus
      />
    </div>
  );
}
