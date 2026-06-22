import { ReactNode } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { ircxBody } from "../lib/ircx";
import { imageEmoji } from "../lib/emoji";

const URL_RE = /(https?:\/\/[^\s<>"']+)/g;

/** Splits text, turning URLs into clickable links that open in the browser. */
function linkify(text: string, keyBase: number): ReactNode[] {
  URL_RE.lastIndex = 0;
  const parts: ReactNode[] = [];
  let last = 0;
  let m: RegExpExecArray | null;
  let i = 0;
  while ((m = URL_RE.exec(text)) !== null) {
    if (m.index > last) parts.push(text.slice(last, m.index));
    const url = m[0];
    parts.push(
      <a
        key={`u${keyBase}_${i++}`}
        className="irc-link"
        href={url}
        onClick={(e) => {
          e.preventDefault();
          openUrl(url).catch(() => {});
        }}
      >
        {url}
      </a>
    );
    last = m.index + url.length;
  }
  if (parts.length === 0) return [text];
  if (last < text.length) parts.push(text.slice(last));
  return parts;
}

// mIRC control codes.
const BOLD = "\x02";
const COLOR = "\x03";
const HEXCOLOR = "\x04";
const RESET = "\x0f";
const REVERSE = "\x16";
const ITALIC = "\x1d";
const STRIKE = "\x1e";
const UNDERLINE = "\x1f";
const MONOSPACE = "\x11";

// The standard mIRC 99-color palette (index -> hex).
// 0-15 classic, 16-98 extended, 99 = default.
const PALETTE: string[] = [
  "#ffffff", "#000000", "#00007f", "#009300", "#ff0000", "#7f0000", "#9c009c", "#fc7f00",
  "#ffff00", "#00fc00", "#009393", "#00ffff", "#0000fc", "#ff00ff", "#7f7f7f", "#d2d2d2",
  "#470000", "#472100", "#474700", "#324700", "#004700", "#00472c", "#004747", "#002747",
  "#000047", "#2e0047", "#470047", "#47002a", "#740000", "#743a00", "#747400", "#517400",
  "#007400", "#007449", "#007474", "#004074", "#000074", "#4b0074", "#740074", "#740045",
  "#b50000", "#b56300", "#b5b500", "#7db500", "#00b500", "#00b571", "#00b5b5", "#0063b5",
  "#0000b5", "#7500b5", "#b500b5", "#b5006b", "#ff0000", "#ff8c00", "#ffff00", "#b2ff00",
  "#00ff00", "#00ffa0", "#00ffff", "#008cff", "#0000ff", "#a500ff", "#ff00ff", "#ff0098",
  "#ff5959", "#ffb459", "#ffff71", "#cfff60", "#6fff6f", "#65ffc9", "#6dffff", "#59b4ff",
  "#5959ff", "#c459ff", "#ff66ff", "#ff59bc", "#ff9c9c", "#ffd39c", "#ffff9c", "#e2ff9c",
  "#9cff9c", "#9cffdb", "#9cffff", "#9cd3ff", "#9c9cff", "#dc9cff", "#ff9cff", "#ff94d3",
  "#000000", "#131313", "#282828", "#363636", "#4d4d4d", "#656565", "#818181", "#9f9f9f",
  "#bcbcbc", "#e2e2e2", "#ffffff",
];

interface Style {
  bold: boolean;
  italic: boolean;
  underline: boolean;
  strike: boolean;
  reverse: boolean;
  mono: boolean;
  fg?: string;
  bg?: string;
}

const EMPTY: Style = {
  bold: false,
  italic: false,
  underline: false,
  strike: false,
  reverse: false,
  mono: false,
};

function colorOf(code: number): string | undefined {
  return code >= 0 && code < PALETTE.length && code !== 99 ? PALETTE[code] : undefined;
}

function styleToCss(s: Style): React.CSSProperties {
  const fg = s.reverse ? s.bg : s.fg;
  const bg = s.reverse ? s.fg : s.bg;
  return {
    fontWeight: s.bold ? 700 : undefined,
    fontStyle: s.italic ? "italic" : undefined,
    textDecoration:
      [s.underline ? "underline" : "", s.strike ? "line-through" : ""].filter(Boolean).join(" ") ||
      undefined,
    fontFamily: s.mono ? "monospace" : undefined,
    color: fg,
    backgroundColor: bg,
  };
}

/** Parses mIRC-formatted text into styled React spans. */
export function parseIrc(raw: string): ReactNode[] {
  const text = ircxBody(raw);
  const emo = imageEmoji();
  const out: ReactNode[] = [];
  let style: Style = { ...EMPTY };
  let run = "";
  let key = 0;

  const flush = () => {
    if (run.length === 0) return;
    const css = styleToCss(style);
    const hasStyle = Object.values(css).some((v) => v !== undefined);
    const children = linkify(run, key);
    out.push(
      hasStyle ? (
        <span key={key++} style={css}>
          {children}
        </span>
      ) : (
        <span key={key++}>{children}</span>
      )
    );
    run = "";
  };

  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    switch (ch) {
      case BOLD:
        flush();
        style = { ...style, bold: !style.bold };
        break;
      case ITALIC:
        flush();
        style = { ...style, italic: !style.italic };
        break;
      case UNDERLINE:
        flush();
        style = { ...style, underline: !style.underline };
        break;
      case STRIKE:
        flush();
        style = { ...style, strike: !style.strike };
        break;
      case REVERSE:
        flush();
        style = { ...style, reverse: !style.reverse };
        break;
      case MONOSPACE:
        flush();
        style = { ...style, mono: !style.mono };
        break;
      case RESET:
        flush();
        style = { ...EMPTY };
        break;
      case COLOR: {
        flush();
        // Parse fg[,bg] numeric codes.
        let j = i + 1;
        const readNum = () => {
          let n = "";
          while (j < text.length && /[0-9]/.test(text[j]) && n.length < 2) {
            n += text[j++];
          }
          return n;
        };
        const fgStr = readNum();
        if (fgStr === "") {
          // Bare color code resets colors.
          style = { ...style, fg: undefined, bg: undefined };
        } else {
          let bgStr = "";
          if (text[j] === "," && /[0-9]/.test(text[j + 1] ?? "")) {
            j++;
            bgStr = readNum();
          }
          style = {
            ...style,
            fg: colorOf(parseInt(fgStr, 10)),
            bg: bgStr ? colorOf(parseInt(bgStr, 10)) : style.bg,
          };
        }
        i = j - 1;
        break;
      }
      case HEXCOLOR: {
        flush();
        const hex = text.slice(i + 1, i + 7);
        if (/^[0-9a-fA-F]{6}$/.test(hex)) {
          style = { ...style, fg: `#${hex}` };
          i += 6;
        } else {
          style = { ...style, fg: undefined, bg: undefined };
        }
        break;
      }
      case ":": {
        // Inline custom image emoji: :code: -> <img>.
        const m = /^:[a-z0-9_+-]+:/i.exec(text.slice(i));
        const url = m ? emo[m[0].toLowerCase()] : undefined;
        if (m && url) {
          flush();
          out.push(<img key={key++} className="emoji-img" src={url} alt={m[0]} title={m[0]} />);
          i += m[0].length - 1;
        } else {
          run += ch;
        }
        break;
      }
      default:
        run += ch;
    }
  }
  flush();
  return out;
}

/** Strips all mIRC formatting codes, returning plain text. */
export function stripFormatting(text: string): string {
  return ircxBody(text)
    .replace(/\x03\d{0,2}(,\d{1,2})?/g, "")
    .replace(/\x04[0-9a-fA-F]{6}/g, "")
    .replace(/[\x02\x0f\x11\x16\x1d\x1e\x1f]/g, "");
}
