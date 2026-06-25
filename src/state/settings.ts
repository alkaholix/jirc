import { create } from "zustand";

export type Theme = "dark" | "light" | "system";
export type Layout = "tree" | "switchbar";

export interface Settings {
  theme: Theme;
  layout: Layout;
  showTimestamps: boolean;
  showJoinPart: boolean;
  notifications: boolean;
  highlightWords: string[];
  /** Nick masks to ignore (wildcards allowed, e.g. "spammer" or "*!*@bad.host"). */
  ignores: string[];
  /** Colour for your own nick (hex). */
  selfNickColor: string;
  /** Custom emoji: `:code:` -> unicode/text, or an image URL (http/https/data). */
  customEmoji: Record<string, string>;
  /** Nicks to watch; you're alerted when they come online/offline. */
  notifyList: string[];
  /** User CSS injected into the app to restyle anything. */
  customCss: string;
  /** Chat font family (empty = theme default) and size in px (0 = default). */
  chatFont: string;
  chatFontSize: number;
  /** Default /quit message when none is given. */
  quitMessage: string;

  // Behaviour / server
  rejoinOnKick: boolean;
  rejoinOnReconnect: boolean;
  keepOpenOnKickQuit: boolean;
  showAway: boolean;
  skipMotd: boolean;
  showPingPong: boolean;
  trace: boolean;

  // DCC networking (for transfers across NAT).
  dccIp: string; // advertised IP; "" = auto (local IP)
  dccPortFrom: number; // listen-port range; 0 = ephemeral
  dccPortTo: number;
}

const DEFAULTS: Settings = {
  theme: "dark",
  layout: "tree",
  showTimestamps: true,
  showJoinPart: true,
  notifications: true,
  highlightWords: [],
  ignores: [],
  selfNickColor: "#7aa2f7",
  customEmoji: {},
  notifyList: [],
  customCss: "",
  chatFont: "",
  chatFontSize: 0,
  quitMessage: "",

  rejoinOnKick: false,
  rejoinOnReconnect: true,
  keepOpenOnKickQuit: true,
  showAway: true,
  skipMotd: false,
  showPingPong: false,
  trace: false,

  dccIp: "",
  dccPortFrom: 0,
  dccPortTo: 0,
};

const STORAGE_KEY = "jirc.settings";

function load(): Settings {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) return { ...DEFAULTS, ...JSON.parse(raw) };
  } catch {
    /* ignore */
  }
  return { ...DEFAULTS };
}

interface SettingsState extends Settings {
  set: <K extends keyof Settings>(key: K, value: Settings[K]) => void;
}

export const useSettings = create<SettingsState>((set) => ({
  ...load(),
  set: (key, value) =>
    set((s) => {
      const next = { ...s, [key]: value };
      const { set: _omit, ...persistable } = next;
      try {
        localStorage.setItem(STORAGE_KEY, JSON.stringify(persistable));
      } catch {
        /* ignore */
      }
      return next;
    }),
}));

/** Applies the chosen theme to the document root. */
export function applyTheme(theme: Theme) {
  const resolved =
    theme === "system"
      ? window.matchMedia("(prefers-color-scheme: light)").matches
        ? "light"
        : "dark"
      : theme;
  document.documentElement.dataset.theme = resolved;
}

/** Applies the chat font family + size as CSS variables (used by .line). */
export function applyChatFont(family: string, size: number) {
  const root = document.documentElement.style;
  if (family.trim()) root.setProperty("--chat-font", family);
  else root.removeProperty("--chat-font");
  if (size > 0) root.setProperty("--chat-size", `${size}px`);
  else root.removeProperty("--chat-size");
}

/** Injects the user's custom CSS into the document (live, persisted). */
export function applyCustomCss(css: string) {
  const id = "jirc-custom-css";
  let el = document.getElementById(id) as HTMLStyleElement | null;
  if (!el) {
    el = document.createElement("style");
    el.id = id;
    document.head.appendChild(el);
  }
  el.textContent = css;
}
