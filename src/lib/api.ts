// Typed wrappers around the Rust `invoke` command surface.
import { invoke } from "@tauri-apps/api/core";

export interface Proxy {
  host: string;
  port: number;
  username?: string;
  password?: string;
}

export interface ServerProfile {
  id?: string;
  name: string;
  host: string;
  port: number;
  tls?: boolean;
  tlsInsecure?: boolean;
  ircx?: boolean;
  sasl?: boolean;
  account?: string;
  accountPassword?: string;
  nickserv?: boolean;
  autoReconnect?: boolean;
  proxy?: Proxy | null;
  nick: string;
  altNick?: string;
  username?: string;
  realname?: string;
  password?: string;
  autojoin: string[];
}

export interface DataLocation {
  /** The folder data is currently stored in (resolved). */
  current: string;
  /** The custom folder saved in Settings (empty = the default per-profile dir). */
  custom: string;
  /** True when an env var or portable install forces the location. */
  forced: boolean;
}

export const api = {
  coreVersion: () => invoke<string>("core_version"),

  // Detachable windows (pop-out / dock-back).
  openDetachedWindow: (label: string, title: string) =>
    invoke("open_detached_window", { label, title }),
  focusWindow: (label: string) => invoke("focus_window", { label }),
  dockWindow: (label: string, bufferKey: string) =>
    invoke("dock_window", { label, bufferKey }),
  closeDetached: (label: string, bufferKey: string) =>
    invoke("close_detached", { label, bufferKey }),

  connect: (profile: ServerProfile) => invoke<string>("irc_connect", { profile }),
  disconnect: (serverId: string, quitMessage?: string) =>
    invoke("irc_disconnect", { serverId, quitMessage }),
  sendRaw: (serverId: string, line: string) => invoke("irc_send_raw", { serverId, line }),
  sendMessage: (serverId: string, target: string, text: string) =>
    invoke("irc_send_message", { serverId, target, text }),
  join: (serverId: string, channel: string) => invoke("irc_join", { serverId, channel }),
  part: (serverId: string, channel: string, reason?: string) =>
    invoke("irc_part", { serverId, channel, reason }),
  setNick: (serverId: string, nick: string) => invoke("irc_set_nick", { serverId, nick }),
  whois: (serverId: string, nick: string) => invoke("irc_whois", { serverId, nick }),
  listConnections: () => invoke<string[]>("irc_list_connections"),

  ircxEnable: (serverId: string, queryOnly = false) =>
    invoke("ircx_enable", { serverId, queryOnly }),
  ircxWhisper: (serverId: string, channel: string, targets: string, text: string) =>
    invoke("ircx_whisper", { serverId, channel, targets, text }),
  ircxPropGet: (serverId: string, object: string, property?: string) =>
    invoke("ircx_prop_get", { serverId, object, property }),
  ircxPropSet: (serverId: string, object: string, property: string, value: string) =>
    invoke("ircx_prop_set", { serverId, object, property, value }),
  ircxListx: (serverId: string, mask?: string) => invoke("ircx_listx", { serverId, mask }),

  profilesLoad: () => invoke<ServerProfile[]>("profiles_load"),
  profilesSave: (profiles: ServerProfile[]) => invoke("profiles_save", { profiles }),
  profilesDelete: (id: string) => invoke("profiles_delete", { id }),
  keyringAvailable: () => invoke<boolean>("keyring_available"),
  dataLocation: () => invoke<DataLocation>("data_location"),
  setDataLocation: (path: string | null) => invoke("set_data_location", { path }),
  logAppend: (network: string, buffer: string, line: string) =>
    invoke("log_append", { network, buffer, line }),
  logRead: (network: string, buffer: string) => invoke<string>("log_read", { network, buffer }),

  scriptsList: () => invoke<string[]>("scripts_list"),
  scriptAddExamples: () => invoke<number>("script_add_examples"),
  scriptRead: (name: string) => invoke<string>("script_read", { name }),
  scriptWrite: (name: string, source: string) => invoke("script_write", { name, source }),
  scriptDelete: (name: string) => invoke("script_delete", { name }),
  scriptPopups: (serverId: string, target: string, myNick: string, network: string, context: string, nick: string) =>
    invoke<PopupItem[]>("script_popups", { serverId, target, myNick, network, context, nick }),
  scriptRunPopup: (
    serverId: string,
    target: string,
    myNick: string,
    network: string,
    command: string,
    params: string[]
  ) => invoke("script_run_popup", { serverId, target, myNick, network, command, params }),
  scriptRunAlias: (
    serverId: string,
    target: string,
    myNick: string,
    network: string,
    name: string,
    args: string
  ) => invoke<boolean>("script_run_alias", { serverId, target, myNick, network, name, args }),
  scriptRunInput: (
    serverId: string,
    target: string,
    myNick: string,
    network: string,
    text: string
  ) => invoke<boolean>("script_run_input", { serverId, target, myNick, network, text }),
  scriptSockets: () => invoke<string[]>("script_sockets"),
  openHelp: () => invoke<void>("open_help"),
  openUrl: (url: string) => invoke<void>("open_url", { url }),
  exitApp: () => invoke<void>("exit_app"),
  dnsLookup: (host: string) => invoke<string[]>("dns_lookup", { host }),
  scriptRunCommand: (
    serverId: string,
    target: string,
    myNick: string,
    network: string,
    command: string,
    args: string
  ) => invoke<void>("script_run_command", { serverId, target, myNick, network, command, args }),
  scriptRunDialog: (
    serverId: string,
    myNick: string,
    network: string,
    dialog: string,
    control: string,
    values: Record<string, string>
  ) => invoke<void>("script_run_dialog", { serverId, myNick, network, dialog, control, values }),
};

/// One control in a script-defined dialog (mirrors the Rust `DialogControl`).
export interface DialogControl {
  kind: string;
  id: string;
  label: string;
  options: string[];
  default: boolean;
  cancel: boolean;
}

// Mirrors the backend `UiEvent` enum (serde tagged by `type`, camelCase).
export type IrcEvent =
  | { type: "connected"; serverId: string }
  | { type: "registered"; serverId: string; nick: string }
  | { type: "disconnected"; serverId: string; reason: string }
  | { type: "raw"; serverId: string; direction: "in" | "out"; line: string }
  | {
      type: "message";
      serverId: string;
      kind: "privmsg" | "notice";
      from: string | null;
      target: string;
      text: string;
      time: string | null;
    }
  | { type: "join"; serverId: string; channel: string; nick: string }
  | { type: "part"; serverId: string; channel: string; nick: string; reason: string | null }
  | {
      type: "quit";
      serverId: string;
      nick: string;
      reason: string | null;
      channels: string[];
    }
  | {
      type: "kick";
      serverId: string;
      channel: string;
      nick: string;
      by: string | null;
      reason: string | null;
      isSelf: boolean;
    }
  | {
      type: "awayChange";
      serverId: string;
      nick: string;
      away: boolean;
      message: string | null;
      channels: string[];
    }
  | { type: "nickChange"; serverId: string; old: string; new: string }
  | { type: "names"; serverId: string; channel: string; members: Member[] }
  | {
      type: "topic";
      serverId: string;
      channel: string;
      topic: string | null;
      setBy: string | null;
    }
  | { type: "mode"; serverId: string; target: string; modes: string; by: string | null }
  | { type: "dialogOpen"; serverId: string; name: string; title: string; controls: DialogControl[] }
  | { type: "dialogClose"; serverId: string; name: string }
  | { type: "dialogSet"; serverId: string; dialog: string; control: string; op: string; value: string }
  | { type: "nickIcon"; serverId: string; nick: string; icon: string }
  | { type: "selfAway"; serverId: string; away: boolean }
  | { type: "numeric"; serverId: string; code: number; args: string[] }
  | { type: "error"; serverId: string; message: string }
  | { type: "echo"; serverId: string; target: string; text: string }
  | { type: "isupport"; serverId: string; chanTypes: string; prefixes: string }
  | { type: "whois"; serverId: string; nick: string; lines: string[] }
  | { type: "listEntry"; serverId: string; channel: string; users: number; topic: string }
  | { type: "listEnd"; serverId: string }
  | { type: "invite"; serverId: string; from: string | null; channel: string }
  | {
      type: "ircxState";
      serverId: string;
      version: string | null;
      packages: string | null;
      maxMessageLength: string | null;
      options: string | null;
    }
  | {
      type: "ircxAccess";
      serverId: string;
      object: string;
      level: string | null;
      mask: string | null;
    }
  | { type: "ircxAccessEnd"; serverId: string; object: string }
  | { type: "ircxProp"; serverId: string; object: string; name: string; value: string }
  | { type: "ircxPropEnd"; serverId: string; object: string }
  | { type: "whisper"; serverId: string; from: string | null; channel: string; text: string }
  | { type: "windowOpen"; serverId: string; name: string; kind: string; title: string }
  | { type: "windowClose"; serverId: string; name: string }
  | { type: "windowLine"; serverId: string; name: string; op: string; n: number; text: string };

export interface Member {
  nick: string;
  prefix: string;
}

export interface PopupItem {
  label: string;
  command: string;
  separator: boolean;
  children: PopupItem[];
}
