import { IrcEvent } from "./api";
import { normalizeAppFontSize, useSettings } from "../state/settings";
import { STATUS, useStore } from "../state/store";
import { api } from "./api";
import { useToolbar } from "../state/toolbar";
import { useChannelCentral } from "../state/channelModes";
import { useAddressBook } from "../state/addressBook";
import { clearTips, routeTipCommand } from "../state/tips";
import { playAlertSound } from "./sound";

export interface ClientCommandActions {
  openConnect?: () => void;
  minimize?: () => void;
  restore?: () => void;
  requestAttention?: () => void;
}

export interface EditboxCommand {
  serverId: string;
  target: string;
  text: string;
  appendSpace: boolean;
  submit: boolean;
  focus: boolean;
  selectionStart?: number;
  selectionEnd?: number;
}

export const EDITBOX_COMMAND_EVENT = "jirc-editbox-command";
export const FINDTEXT_COMMAND_EVENT = "jirc-findtext-command";

export interface FindTextCommand {
  serverId: string;
  target: string;
  text: string;
  next: boolean;
}

function words(args: string): string[] {
  return args.trim().match(/"[^"]*"|\S+/g)?.map((word) =>
    word.startsWith('"') && word.endsWith('"') ? word.slice(1, -1) : word
  ) ?? [];
}

function wildcard(mask: string, value: string): boolean {
  const escaped = mask.replace(/[.+^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`^${escaped.replace(/\*/g, ".*").replace(/\?/g, ".")}$`, "i").test(value);
}

export function parseEditboxCommand(
  serverId: string,
  currentTarget: string,
  args: string
): EditboxCommand {
  const parts = words(args);
  let switches = "";
  if (parts[0]?.startsWith("-")) switches = parts.shift()!.slice(1);
  let target = currentTarget;
  if (switches.includes("s")) target = STATUS;
  else if (
    !switches.includes("a") &&
    parts.length > 1 &&
    /^(?:[#&+!%@=]|\(status\)$|status window$)/i.test(parts[0])
  ) {
    target = parts.shift()!;
  }

  const start = switches.match(/b(\d+)/i);
  const end = switches.match(/e(\d+)/i);
  return {
    serverId,
    target: target === "Status Window" ? STATUS : target,
    text: parts.join(" "),
    appendSpace: switches.includes("p"),
    submit: switches.includes("n"),
    focus: /f[12]?/i.test(switches) || switches.includes("a"),
    selectionStart: start ? Number(start[1]) : undefined,
    selectionEnd: end ? Number(end[1]) : undefined,
  };
}

function routeClearAll(serverId: string, args: string) {
  const switches = words(args)[0]?.replace(/^-/, "") ?? "";
  const allConnections = switches.includes("a");
  const selected = switches.replace("a", "");
  const allKinds = selected.length === 0;
  useStore.setState((state) => {
    const buffers = { ...state.buffers };
    for (const [key, buffer] of Object.entries(buffers)) {
      if (!allConnections && buffer.serverId !== serverId) continue;
      const matches =
        allKinds ||
        (selected.includes("s") && buffer.kind === "status") ||
        (selected.includes("n") && buffer.kind === "channel") ||
        (/[qm]/.test(selected) && buffer.kind === "query" && !buffer.name.startsWith("=")) ||
        (selected.includes("t") && buffer.kind === "query" && buffer.name.startsWith("=")) ||
        (selected.includes("u") && buffer.kind === "window");
      if (matches) buffers[key] = { ...buffer, lines: [] };
    }
    return { buffers };
  });
}

function routeClose(serverId: string, args: string) {
  const parts = words(args);
  const switches = parts[0]?.startsWith("-") ? parts.shift()!.slice(1) : "";
  const allConnections = switches.includes("a");
  const names = parts;
  const state = useStore.getState();
  const closeKeys = state.order.filter((key) => {
    const buffer = state.buffers[key];
    if (!buffer || (!allConnections && buffer.serverId !== serverId)) return false;
    const kindMatches =
      (!switches && names.length > 0) ||
      (switches.includes("m") && buffer.kind === "query" && !buffer.name.startsWith("=")) ||
      (switches.includes("c") && buffer.kind === "query" && buffer.name.startsWith("=")) ||
      (switches.includes("@") && buffer.kind === "window") ||
      (switches.includes("t") && buffer.kind === "status");
    return kindMatches && (names.length === 0 || names.some((name) => wildcard(name, buffer.name)));
  });
  for (const key of closeKeys) {
    const buffer = useStore.getState().buffers[key];
    if (buffer?.kind === "status") useStore.getState().closeServer(buffer.serverId);
    else useStore.getState().closeBuffer(key);
  }
}

function routeFont(args: string, openSettings: () => void) {
  const parts = words(args);
  if (parts.length === 0) {
    openSettings();
    return;
  }
  const switches = parts[0]?.startsWith("-") ? parts.shift()!.slice(1) : "";
  if (switches.includes("z") && parts.length === 0) {
    useSettings.getState().set("chatFont", "");
    useSettings.getState().set("chatFontSize", 0);
    return;
  }
  const sizeIndex = parts.findIndex((part) => /^-?\d+$/.test(part));
  if (sizeIndex >= 0) {
    useSettings.getState().set(
      "chatFontSize",
      normalizeAppFontSize(Number(parts[sizeIndex]))
    );
    const family = parts.slice(sizeIndex + 1).join(" ");
    if (family) useSettings.getState().set("chatFont", family);
  }
}

function routeMarkAsRead(serverId: string, args: string) {
  const rawName = args.trim();
  const name = /^(?:status window|\(status\))$/i.test(rawName) ? STATUS : rawName;
  useStore.setState((state) => {
    const buffers = { ...state.buffers };
    for (const [key, buffer] of Object.entries(buffers)) {
      if (buffer.serverId !== serverId) continue;
      if (name && buffer.name.toLowerCase() !== name.toLowerCase()) continue;
      buffers[key] = { ...buffer, unread: 0, mention: false };
    }
    return { buffers };
  });
}

function routeStrip(args: string) {
  let enabled = new Set(useSettings.getState().stripCodes);
  let adding = true;
  for (const ch of args.toLowerCase()) {
    if (ch === "+") adding = true;
    else if (ch === "-") adding = false;
    else if ("buriec".includes(ch)) adding ? enabled.add(ch) : enabled.delete(ch);
  }
  useSettings.getState().set("stripCodes", "buriec".split("").filter((ch) => enabled.has(ch)).join(""));
}

function routeQueryBroadcast(serverId: string, args: string, action: boolean) {
  if (!args) return;
  const state = useStore.getState();
  const nick = state.servers[serverId]?.nick ?? "me";
  for (const buffer of Object.values(state.buffers)) {
    if (buffer.serverId !== serverId || buffer.kind !== "query" || buffer.name.startsWith("=")) continue;
    const sent = action
      ? api.sendRaw(serverId, `PRIVMSG ${buffer.name} :\x01ACTION ${args}\x01`)
      : api.sendMessage(serverId, buffer.name, args);
    sent.then(() => useStore.getState().appendLine(serverId, buffer.name, "query", {
      kind: action ? "action" : "msg", from: nick, text: args, self: true,
    })).catch(() => {});
  }
}

function routeDelayedPrivilege(serverId: string, currentTarget: string, args: string, mode: "o" | "v") {
  const parts = words(args);
  const delay = Number(parts.shift());
  if (!Number.isFinite(delay) || delay < 0) return;
  const chanTypes = useStore.getState().servers[serverId]?.chanTypes ?? "#&+!";
  const channel = parts[0] && chanTypes.includes(parts[0][0]) ? parts.shift()! : currentTarget;
  const nick = parts[0];
  if (!channel || !nick) return;
  window.setTimeout(() => {
    const state = useStore.getState();
    const server = state.servers[serverId];
    const buffer = Object.values(state.buffers).find((b) =>
      b.serverId === serverId && b.kind === "channel" && b.name.toLowerCase() === channel.toLowerCase()
    );
    const member = buffer?.members.find((m) => m.nick.toLowerCase() === nick.toLowerCase());
    const prefixAt = server?.prefixModes.indexOf(mode) ?? -1;
    const symbol = prefixAt >= 0 ? server?.prefixes[prefixAt] : mode === "o" ? "@" : "+";
    if (member && symbol && !member.prefix.includes(symbol)) {
      api.sendRaw(serverId, `MODE ${channel} +${mode} ${nick}`).catch(() => {});
    }
  }, delay * 1000);
}

export function routeClientCommand(
  event: Extract<IrcEvent, { type: "clientCommand" }>,
  openSettings: () => void,
  actions: ClientCommandActions = {}
) {
  switch (event.command) {
    case "ajinvite": {
      const mode = words(event.args)[0]?.toLowerCase();
      if (mode === "on" || mode === "off") useSettings.getState().set("autoJoinInvites", mode === "on");
      break;
    }
    case "beep":
      playAlertSound("mention", true);
      break;
    case "bindip":
      useSettings.getState().set("dccBindIp", words(event.args)[0] ?? "");
      break;
    case "clipboard": {
      const text = event.args.replace(/^-[asn]\s+/i, "");
      if (text && navigator.clipboard?.writeText) void navigator.clipboard.writeText(text).catch(() => {});
      break;
    }
    case "cnick": {
      const parts = words(event.args).filter((part) => !part.startsWith("-"));
      const nick = parts[0] ?? "";
      const serverNick = useStore.getState().servers[event.serverId]?.nick ?? "";
      const numeric = Number(parts[1]);
      if (nick.toLowerCase() === serverNick.toLowerCase() && Number.isFinite(numeric)) {
        const value = Math.max(0, Math.min(0xffffff, numeric >>> 0));
        const red = value & 0xff;
        const green = (value >>> 8) & 0xff;
        const blue = (value >>> 16) & 0xff;
        useSettings.getState().set("selfNickColor", `#${red.toString(16).padStart(2, "0")}${green.toString(16).padStart(2, "0")}${blue.toString(16).padStart(2, "0")}`);
      }
      break;
    }
    case "color":
      openSettings();
      break;
    case "background": {
      const value = event.args.replace(/^-[a-z]+\s+/i, "").trim();
      if (!value || value === "none") document.documentElement.style.removeProperty("background-image");
      else if (/^#(?:[0-9a-f]{3}|[0-9a-f]{6})$/i.test(value)) document.documentElement.style.backgroundColor = value;
      else document.documentElement.style.backgroundImage = `url(${JSON.stringify(value)})`;
      break;
    }
    case "donotdisturb": {
      const mode = words(event.args)[0]?.toLowerCase();
      useSettings.getState().set("quietHoursEnabled", mode !== "off");
      break;
    }
    case "dccignore": {
      const mode = words(event.args)[0]?.toLowerCase();
      useSettings.getState().set("dccIgnore", mode === "on" || mode === "1" || mode === "+");
      break;
    }
    case "creq": {
      const mode = words(event.args)[0]?.toLowerCase();
      if (mode === "ask" || mode === "auto" || mode === "ignore") useSettings.getState().set("dccChatRequest", mode);
      break;
    }
    case "sreq": {
      const mode = words(event.args)[0]?.toLowerCase();
      if (mode === "ask" || mode === "auto" || mode === "ignore") useSettings.getState().set("dccSendRequest", mode);
      break;
    }
    case "debug": {
      const mode = words(event.args)[0]?.toLowerCase();
      useSettings.getState().set("trace", mode === "on" || mode === "1");
      break;
    }
    case "dqwindow":
    case "flist":
      useStore.getState().setActive(useStore.getState().ensureBuffer(event.serverId, "DCC Transfers", "window"));
      break;
    case "firewall":
    case "proxy":
    case "perform":
      actions.openConnect?.();
      break;
    case "flood": {
      const parts = words(event.args);
      const mode = parts.shift()?.toLowerCase();
      const enabled = mode !== "off";
      const messages = Math.max(1, Number(parts[0]) || useSettings.getState().floodMessages);
      const seconds = Math.max(1, Number(parts[1]) || useSettings.getState().floodSeconds);
      useSettings.getState().set("floodEnabled", enabled);
      useSettings.getState().set("floodMessages", messages);
      useSettings.getState().set("floodSeconds", seconds);
      void api.configureFlood(enabled, messages, seconds).catch(() => {});
      break;
    }
    case "flash":
      actions.requestAttention?.();
      break;
    case "setlayer": {
      const opacity = Math.max(0, Math.min(255, Number(words(event.args).at(-1)) || 255));
      document.documentElement.style.opacity = String(opacity / 255);
      break;
    }
    case "mdi": {
      const mode = words(event.args)[0]?.toLowerCase();
      if (mode === "tree" || mode === "-t") useSettings.getState().set("layout", "tree");
      else if (mode === "switchbar" || mode === "-s") useSettings.getState().set("layout", "switchbar");
      break;
    }
    case "tray":
      actions.minimize?.();
      break;
    case "showmirc":
      actions.restore?.();
      break;
    case "vol": {
      const value = Math.max(0, Math.min(100, Number(words(event.args).at(-1)) || 0));
      useSettings.getState().set("soundVolume", value / 100);
      break;
    }
    case "abook": {
      const nick = words(event.args).find((part) => !part.startsWith("-")) ?? "";
      const network = useStore.getState().servers[event.serverId]?.name ?? "";
      void useAddressBook.getState().show(nick, network);
      break;
    }
    case "channel": {
      const requested = words(event.args)[0] ?? "";
      const chanTypes = useStore.getState().servers[event.serverId]?.chanTypes ?? "#&!+%";
      const channel = requested && chanTypes.includes(requested[0])
        ? requested
        : chanTypes.includes(event.currentTarget[0]) ? event.currentTarget : "";
      if (channel) useChannelCentral.getState().open(event.serverId, channel);
      break;
    }
    case "markasread":
      routeMarkAsRead(event.serverId, event.args);
      break;
    case "menubar": {
      const mode = words(event.args)[0]?.toLowerCase();
      if (mode === "on" || mode === "off") useSettings.getState().set("menubarVisible", mode === "on");
      break;
    }
    case "tips": {
      const mode = words(event.args)[0]?.toLowerCase();
      if (mode === "on" || mode === "off") {
        useSettings.getState().set("tipsEnabled", mode === "on");
        if (mode === "off") clearTips();
      }
      break;
    }
    case "tip-create":
    case "tip-close":
    case "tip-update":
      if (useSettings.getState().tipsEnabled || event.command === "tip-close") {
        routeTipCommand(event.command, event.args, {
          serverId: event.serverId,
          target: event.currentTarget,
        });
      }
      break;
    case "strip":
      routeStrip(event.args);
      break;
    case "qmsg":
    case "qme":
      routeQueryBroadcast(event.serverId, event.args, event.command === "qme");
      break;
    case "pop":
    case "pvoice":
      routeDelayedPrivilege(event.serverId, event.currentTarget, event.args, event.command === "pop" ? "o" : "v");
      break;
    case "toolbar": {
      const mode = words(event.args)[0]?.toLowerCase();
      if (mode === "on" || mode === "off") useToolbar.getState().setVisible(mode === "on");
      break;
    }
    case "editbox":
      window.dispatchEvent(
        new CustomEvent<EditboxCommand>(EDITBOX_COMMAND_EVENT, {
          detail: parseEditboxCommand(event.serverId, event.currentTarget, event.args),
        })
      );
      break;
    case "findtext": {
      const parts = words(event.args);
      const switches = parts[0]?.startsWith("-") ? parts.shift()!.slice(1) : "";
      window.dispatchEvent(new CustomEvent<FindTextCommand>(FINDTEXT_COMMAND_EVENT, {
        detail: { serverId: event.serverId, target: event.currentTarget, text: parts.join(" "), next: switches.includes("n") },
      }));
      break;
    }
    case "timestamp": {
      const mode = words(event.args).find((word) =>
        ["on", "off", "default", "inline", "divider"].includes(word.toLowerCase())
      )?.toLowerCase();
      if (mode === "off") useSettings.getState().set("timestampMode", "off");
      else if (mode === "divider") useSettings.getState().set("timestampMode", "divider");
      else if (mode === "on" || mode === "inline" || mode === "default") {
        useSettings.getState().set("timestampMode", "inline");
      }
      break;
    }
    case "switchbar": {
      const mode = words(event.args)[0]?.toLowerCase();
      if (mode === "on") useSettings.getState().set("layout", "switchbar");
      else if (mode === "off") useSettings.getState().set("layout", "tree");
      break;
    }
    case "treebar": {
      const parts = words(event.args);
      const mode = parts[0]?.toLowerCase();
      if (mode === "on") useSettings.getState().set("layout", "tree");
      else if (mode === "off") useSettings.getState().set("layout", "switchbar");
      const widthAt = parts.findIndex((part) => part.toLowerCase() === "-w");
      if (widthAt >= 0) {
        const width = Math.max(140, Math.min(600, Number(parts[widthAt + 1]) || 220));
        useSettings.getState().set("treebarWidth", width);
      }
      if (parts.some((part) => part.toLowerCase() === "-l")) useSettings.getState().set("treebarPosition", "left");
      if (parts.some((part) => part.toLowerCase() === "-r")) useSettings.getState().set("treebarPosition", "right");
      break;
    }
    case "linesep": {
      const parts = words(event.args);
      const explicitTarget = parts[0] && (/^[#&+%!@=]/.test(parts[0]) || parts[0] === STATUS);
      const target = explicitTarget ? parts.shift()! : event.currentTarget || STATUS;
      const server = useStore.getState().servers[event.serverId];
      const kind = target === STATUS ? "status" : server?.chanTypes.includes(target[0]) ? "channel" : target.startsWith("@") ? "window" : "query";
      useStore.getState().appendLine(event.serverId, target, kind, { kind: "separator", text: parts.join(" ") });
      break;
    }
    case "font":
      routeFont(event.args, openSettings);
      break;
    case "clearall":
      routeClearAll(event.serverId, event.args);
      break;
    // `/exit` quits jIRC. `/disconnect` drops this connection only, with an
    // optional quit message, and leaves the client running.
    case "exit":
      void api.exitApp();
      break;
    case "disconnect": {
      const message = event.args.trim();
      void api.disconnect(event.serverId, message || undefined);
      break;
    }
    case "close":
      routeClose(event.serverId, event.args);
      break;
    case "queryrn": {
      const [oldName, newName] = words(event.args);
      const state = useStore.getState();
      const key = state.order.find((candidate) => {
        const buffer = state.buffers[candidate];
        return buffer?.serverId === event.serverId && buffer.kind === "query" && buffer.name.toLowerCase() === oldName?.toLowerCase();
      });
      if (key && newName) state.renameBuffer(key, newName);
      break;
    }
    case "help":
      api.openHelp(words(event.args)[0]).catch(() => {});
      break;
    case "log": {
      const parts = words(event.args);
      const mode = parts.find((part) => /^(on|off)$/i.test(part))?.toLowerCase();
      const target = parts.find((part) => !part.startsWith("-") && !/^(on|off)$/i.test(part)) ?? event.currentTarget;
      useStore.setState((state) => {
        const buffers = { ...state.buffers };
        for (const [key, buffer] of Object.entries(buffers)) {
          if (buffer.serverId === event.serverId && buffer.name.toLowerCase() === target.toLowerCase()) {
            buffers[key] = { ...buffer, logging: mode !== "off" };
          }
        }
        return { buffers };
      });
      break;
    }
    case "logview": {
      const target = words(event.args).find((part) => !part.startsWith("-")) ?? event.currentTarget;
      const state = useStore.getState();
      state.setActive(state.ensureBuffer(event.serverId, target, target.startsWith("#") ? "channel" : "query"));
      break;
    }
  }
}
