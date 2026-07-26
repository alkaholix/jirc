import { create } from "zustand";

export type Theme = "dark" | "light" | "system";
export type ScriptTheme = "vscode-dark" | "vscode-light" | "monokai" | "solarized-dark";
export type TimestampMode = "inline" | "divider" | "off";
export type Layout = "tree" | "switchbar";

export interface Settings {
  theme: Theme;
  /** Colour theme used by the mSL script editor. */
  scriptTheme: ScriptTheme;
  layout: Layout;
  timestampMode: TimestampMode;
  showJoinPart: boolean;
  notifications: boolean;
  soundEnabled: boolean;
  soundVolume: number;
  mentionSound: string;
  privateSound: string;
  inviteSound: string;
  onlineSound: string;
  quietHoursEnabled: boolean;
  quietHoursFrom: string;
  quietHoursTo: string;
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
  /** Show mIRC bold/italic/underline/colour controls beside the message input. */
  showInputToolbar: boolean;
  /** Use the platform WebView's installed spell-check dictionary in message inputs. */
  spellCheck: boolean;
  /** BCP-47 language tag; empty follows the operating-system language. */
  spellCheckLanguage: string;
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
  /** Rate-limit user/script IRC lines to avoid excess-flood disconnects. */
  floodEnabled: boolean;
  floodMessages: number;
  floodSeconds: number;

  // DCC networking (for transfers across NAT).
  dccIp: string; // advertised IP; "" = auto (local IP)
  dccPortFrom: number; // listen-port range; 0 = ephemeral
  dccPortTo: number;
  /** Use mIRC's passive/reverse DCC negotiation for outgoing offers. */
  dccPassive: boolean;
  dccServerEnabled: boolean;
  dccServerPort: number;
  dccServerChat: boolean;
  dccServerSend: boolean;
  dccServerFserve: boolean;
}

const DEFAULTS: Settings = {
  theme: "dark",
  scriptTheme: "vscode-dark",
  layout: "tree",
  timestampMode: "inline",
  showJoinPart: true,
  notifications: true,
  soundEnabled: true,
  soundVolume: 0.5,
  mentionSound: "",
  privateSound: "",
  inviteSound: "",
  onlineSound: "",
  quietHoursEnabled: false,
  quietHoursFrom: "22:00",
  quietHoursTo: "07:00",
  highlightWords: [],
  ignores: [],
  selfNickColor: "#7aa2f7",
  customEmoji: {},
  notifyList: [],
  customCss: "",
  chatFont: "",
  chatFontSize: 0,
  showInputToolbar: true,
  spellCheck: true,
  spellCheckLanguage: "",
  quitMessage: "",

  rejoinOnKick: false,
  rejoinOnReconnect: true,
  keepOpenOnKickQuit: true,
  showAway: true,
  skipMotd: false,
  showPingPong: false,
  trace: false,
  floodEnabled: true,
  floodMessages: 4,
  floodSeconds: 2,

  dccIp: "",
  dccPortFrom: 0,
  dccPortTo: 0,
  dccPassive: false,
  dccServerEnabled: false,
  dccServerPort: 59,
  dccServerChat: true,
  dccServerSend: true,
  dccServerFserve: true,
};

const STORAGE_KEY = "jirc.settings";

export function normalizeSavedSettings(
  value: Record<string, unknown>
): Settings {
  const { showTimestamps, ...saved } = value;
  return {
    ...DEFAULTS,
    ...saved,
    timestampMode:
      (saved.timestampMode as TimestampMode | undefined) ??
      (showTimestamps === false ? "off" : "inline"),
  } as Settings;
}

function load(): Settings {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      return normalizeSavedSettings(JSON.parse(raw));
    }
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

/** Resolves the chosen theme to the colours currently shown by the client. */
export function resolveTheme(theme: Theme): "dark" | "light" {
  return (
    theme === "system"
      ? window.matchMedia("(prefers-color-scheme: light)").matches
        ? "light"
        : "dark"
      : theme
  );
}

/** Applies the chosen theme to the document root. */
export function applyTheme(theme: Theme) {
  const resolved = resolveTheme(theme);
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
