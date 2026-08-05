import { create } from "zustand";

export type Theme = "dark" | "light" | "system";
export type ScriptTheme = "vscode-dark" | "vscode-light" | "monokai" | "solarized-dark";
export type TimestampMode = "inline" | "divider" | "off";
export type Layout = "tree" | "switchbar";
export type DockPaneId = "treebar" | "nicklist" | "panels";
export type DockSide = "left" | "right";
export type UrlPreviewStyle = "compact" | "rich" | "image";

export interface Settings {
  theme: Theme;
  /** Colour theme used by the mSL script editor. */
  scriptTheme: ScriptTheme;
  layout: Layout;
  menubarVisible: boolean;
  tipsEnabled: boolean;
  /** Use OS-level script popup menus; WebView menus remain the fallback. */
  nativePopupMenus: boolean;
  treebarWidth: number;
  treebarPosition: "left" | "right";
  nicklistWidth: number;
  panelsWidth: number;
  dockPaneOrder: DockPaneId[];
  dockPaneSides: Record<DockPaneId, DockSide>;
  timestampMode: TimestampMode;
  /** mIRC /strip flags currently enabled (b/u/r/i/e/c). */
  stripCodes: string;
  showJoinPart: boolean;
  /** Show safe metadata cards beneath HTTP(S) links in chat messages. */
  urlPreviews: boolean;
  urlPreviewStyle: UrlPreviewStyle;
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
  /** Application font family (empty = theme default) and size in px (0 = default). */
  chatFont: string;
  chatFontSize: number;
  /** Show mIRC bold/italic/underline/colour controls beside the message input. */
  showInputToolbar: boolean;
  /** Use the platform WebView's installed spell-check dictionary in message inputs. */
  spellCheck: boolean;
  /** Correct a conservative list of common typing mistakes as a word is completed. */
  autoCorrect: boolean;
  /** BCP-47 language tag; empty follows the operating-system language. */
  spellCheckLanguage: string;
  /** Default /quit message when none is given. */
  quitMessage: string;

  // Behaviour / server
  rejoinOnKick: boolean;
  /** Automatically join channels when invited (`/ajinvite`). */
  autoJoinInvites: boolean;
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
  dccBindIp: string; // local listener interface; "" = all interfaces
  dccPortFrom: number; // listen-port range; 0 = ephemeral
  dccPortTo: number;
  /** Use mIRC's passive/reverse DCC negotiation for outgoing offers. */
  dccPassive: boolean;
  dccIgnore: boolean;
  dccChatRequest: "ask" | "auto" | "ignore";
  dccSendRequest: "ask" | "auto" | "ignore";
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
  menubarVisible: true,
  tipsEnabled: true,
  nativePopupMenus: false,
  treebarWidth: 220,
  treebarPosition: "left",
  nicklistWidth: 180,
  panelsWidth: 240,
  dockPaneOrder: ["treebar", "nicklist", "panels"],
  dockPaneSides: { treebar: "left", nicklist: "right", panels: "right" },
  timestampMode: "inline",
  stripCodes: "",
  showJoinPart: true,
  urlPreviews: true,
  urlPreviewStyle: "compact",
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
  autoCorrect: false,
  spellCheckLanguage: "",
  quitMessage: "",

  rejoinOnKick: false,
  autoJoinInvites: false,
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
  dccBindIp: "",
  dccPortFrom: 0,
  dccPortTo: 0,
  dccPassive: false,
  dccIgnore: false,
  dccChatRequest: "ask",
  dccSendRequest: "ask",
  dccServerEnabled: false,
  dccServerPort: 59,
  dccServerChat: true,
  dccServerSend: true,
  dccServerFserve: true,
};

const STORAGE_KEY = "jirc.settings";
export const MIN_APP_FONT_SIZE = 8;

export function normalizeAppFontSize(size: number): number {
  if (!Number.isFinite(size) || size === 0) return 0;
  return Math.max(MIN_APP_FONT_SIZE, Math.abs(size));
}

export function normalizeSavedSettings(
  value: Record<string, unknown>
): Settings {
  const { showTimestamps, ...saved } = value;
  const settings = {
    ...DEFAULTS,
    ...saved,
    timestampMode:
      (saved.timestampMode as TimestampMode | undefined) ??
      (showTimestamps === false ? "off" : "inline"),
  } as Settings;
  settings.chatFontSize = normalizeAppFontSize(settings.chatFontSize);
  if (!["compact", "rich", "image"].includes(settings.urlPreviewStyle)) {
    settings.urlPreviewStyle = "compact";
  }
  const validPanes: DockPaneId[] = ["treebar", "nicklist", "panels"];
  const savedOrder = Array.isArray(saved.dockPaneOrder)
    ? saved.dockPaneOrder.filter((pane): pane is DockPaneId => validPanes.includes(pane as DockPaneId))
    : [];
  const savedSides = saved.dockPaneSides && typeof saved.dockPaneSides === "object"
    ? saved.dockPaneSides as Partial<Record<DockPaneId, unknown>>
    : {};
  settings.dockPaneOrder = [...new Set([...savedOrder, ...validPanes])];
  settings.dockPaneSides = {
    treebar: savedSides.treebar === "right" || settings.treebarPosition === "right" ? "right" : "left",
    nicklist: savedSides.nicklist === "left" ? "left" : "right",
    panels: savedSides.panels === "left" ? "left" : "right",
  };
  settings.treebarWidth = Math.max(140, Math.min(600, Number(settings.treebarWidth) || 220));
  settings.nicklistWidth = Math.max(120, Math.min(500, Number(settings.nicklistWidth) || 180));
  settings.panelsWidth = Math.max(160, Math.min(600, Number(settings.panelsWidth) || 240));
  return settings;
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

/** Applies the chosen font family and size throughout the application. */
export function applyChatFont(family: string, size: number) {
  const root = document.documentElement.style;
  if (family.trim()) root.setProperty("--app-font", family);
  else root.removeProperty("--app-font");
  const normalizedSize = normalizeAppFontSize(size);
  if (normalizedSize > 0) root.setProperty("--app-font-size", `${normalizedSize}px`);
  else root.removeProperty("--app-font-size");
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
