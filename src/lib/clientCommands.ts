import { IrcEvent } from "./api";
import { normalizeAppFontSize, useSettings } from "../state/settings";
import { STATUS, useStore } from "../state/store";

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

export function routeClientCommand(
  event: Extract<IrcEvent, { type: "clientCommand" }>,
  openSettings: () => void
) {
  switch (event.command) {
    case "editbox":
      window.dispatchEvent(
        new CustomEvent<EditboxCommand>(EDITBOX_COMMAND_EVENT, {
          detail: parseEditboxCommand(event.serverId, event.currentTarget, event.args),
        })
      );
      break;
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
      const mode = words(event.args)[0]?.toLowerCase();
      if (mode === "on") useSettings.getState().set("layout", "tree");
      else if (mode === "off") useSettings.getState().set("layout", "switchbar");
      break;
    }
    case "font":
      routeFont(event.args, openSettings);
      break;
    case "clearall":
      routeClearAll(event.serverId, event.args);
      break;
    case "close":
      routeClose(event.serverId, event.args);
      break;
  }
}
