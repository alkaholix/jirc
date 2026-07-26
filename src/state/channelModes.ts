import { create } from "zustand";
import { IrcEvent } from "../lib/api";
import { serverBufferKey, useStore } from "./store";

export interface ChanModeSpec {
  list: string;
  alwaysArg: string;
  setArg: string;
  flags: string;
}

export interface ChanModes {
  /** Active non-parameter modes. */
  flags: Set<string>;
  /** Active parameter modes and their current values. */
  values: Record<string, string>;
}

interface ModeState {
  byBuffer: Record<string, ChanModes>;
}

interface ListState {
  /** Lists are indexed by buffer key, then mode letter (`b`, `e`, `I`, …). */
  byBuffer: Record<string, Record<string, string[]>>;
}

export const useChannelModes = create<ModeState>(() => ({ byBuffer: {} }));
export const useChannelLists = create<ListState>(() => ({ byBuffer: {} }));

export function parseChanModeSpec(raw: string): ChanModeSpec {
  const [list = "beI", alwaysArg = "k", setArg = "l", flags = "imnpstrS"] =
    raw.split(",");
  return { list, alwaysArg, setArg, flags };
}

export function emptyModes(): ChanModes {
  return { flags: new Set(), values: {} };
}

function modeTakesArgument(
  spec: ChanModeSpec,
  mode: string,
  adding: boolean,
  prefixModes = ""
): boolean {
  return (
    prefixModes.includes(mode) ||
    spec.list.includes(mode) ||
    spec.alwaysArg.includes(mode) ||
    (adding && spec.setArg.includes(mode))
  );
}

/** Apply one MODE delta according to the server's advertised CHANMODES groups. */
export function applyModeDelta(
  current: ChanModes,
  modeString: string,
  params: string[],
  spec: ChanModeSpec,
  prefixModes = ""
): {
  modes: ChanModes;
  listOps: Array<{ mode: string; adding: boolean; mask: string }>;
} {
  const flags = new Set(current.flags);
  const values = { ...current.values };
  const listOps: Array<{ mode: string; adding: boolean; mask: string }> = [];
  let adding = true;
  let paramIndex = 0;

  for (const mode of modeString) {
    if (mode === "+" || mode === "-") {
      adding = mode === "+";
      continue;
    }
    const argument = modeTakesArgument(spec, mode, adding, prefixModes)
      ? params[paramIndex++] ?? ""
      : "";
    if (prefixModes.includes(mode)) {
      // Membership privileges always carry a nick and live in the nick list,
      // not the channel's ordinary mode state.
    } else if (spec.list.includes(mode)) {
      if (argument) listOps.push({ mode, adding, mask: argument });
    } else if (spec.alwaysArg.includes(mode) || spec.setArg.includes(mode)) {
      if (adding) values[mode] = argument;
      else delete values[mode];
    } else if (adding) {
      flags.add(mode);
    } else {
      flags.delete(mode);
    }
  }
  return { modes: { flags, values }, listOps };
}

export interface ModeChange {
  mode: string;
  adding: boolean;
  argument?: string;
}

/** Packs mode operations without exceeding ISUPPORT MODES per command. */
export function packModeChanges(
  channel: string,
  changes: ModeChange[],
  modesPerLine: number
): string[] {
  const limit = Math.max(1, modesPerLine || 1);
  const lines: string[] = [];
  for (let offset = 0; offset < changes.length; offset += limit) {
    const batch = changes.slice(offset, offset + limit);
    let modeString = "";
    let sign = "";
    const params: string[] = [];
    for (const change of batch) {
      const nextSign = change.adding ? "+" : "-";
      if (nextSign !== sign) {
        modeString += nextSign;
        sign = nextSign;
      }
      modeString += change.mode;
      if (change.argument !== undefined) params.push(change.argument);
    }
    lines.push(`MODE ${channel} ${modeString}${params.length ? ` ${params.join(" ")}` : ""}`);
  }
  return lines;
}

function setModes(key: string, modes: ChanModes): void {
  useChannelModes.setState((state) => ({
    byBuffer: { ...state.byBuffer, [key]: modes },
  }));
}

function updateList(key: string, mode: string, mask: string, adding: boolean): void {
  useChannelLists.setState((state) => {
    const lists = state.byBuffer[key] ?? {};
    const current = lists[mode] ?? [];
    const next = adding
      ? current.some((item) => item.toLowerCase() === mask.toLowerCase())
        ? current
        : [...current, mask]
      : current.filter((item) => item.toLowerCase() !== mask.toLowerCase());
    return {
      byBuffer: {
        ...state.byBuffer,
        [key]: { ...lists, [mode]: next },
      },
    };
  });
}

export function clearChannelLists(serverId: string, channel: string, modes: string): void {
  const key = serverBufferKey(serverId, channel);
  useChannelLists.setState((state) => {
    const lists = { ...(state.byBuffer[key] ?? {}) };
    for (const mode of modes) lists[mode] = [];
    return { byBuffer: { ...state.byBuffer, [key]: lists } };
  });
}

export function channelList(serverId: string, channel: string, mode: string): string[] {
  return (
    useChannelLists.getState().byBuffer[serverBufferKey(serverId, channel)]?.[mode] ?? []
  );
}

const NUMERIC_LIST_MODES: Record<number, string> = {
  346: "I", // invite exception
  348: "e", // ban exception
  367: "b", // ban
};

/** Routes live MODE, RPL_CHANNELMODEIS, and standard list numerics. */
export function routeModeEvent(event: IrcEvent): void {
  if (!("serverId" in event)) return;
  const server = useStore.getState().servers[event.serverId];
  const chanTypes = server?.chanTypes ?? "#&!+%";
  const spec = parseChanModeSpec(server?.chanModes ?? "beI,k,l,imnpstrS");
  const prefixModes = server?.prefixModes ?? "qaohv";

  if (event.type === "mode" && event.target && chanTypes.includes(event.target[0])) {
    const [modeString, ...params] = event.modes.split(" ");
    const key = serverBufferKey(event.serverId, event.target);
    const result = applyModeDelta(
      useChannelModes.getState().byBuffer[key] ?? emptyModes(),
      modeString,
      params,
      spec,
      prefixModes
    );
    setModes(key, result.modes);
    for (const operation of result.listOps) {
      updateList(key, operation.mode, operation.mask, operation.adding);
    }
    return;
  }

  if (event.type !== "numeric") return;
  if (event.code === 324) {
    const [, channel, modeString = "", ...params] = event.args;
    if (channel) {
      setModes(
        serverBufferKey(event.serverId, channel),
        applyModeDelta(emptyModes(), modeString, params, spec).modes
      );
    }
    return;
  }
  const listMode = event.code === 728 ? "q" : NUMERIC_LIST_MODES[event.code];
  const [, channel] = event.args;
  // Common RPL_QUIETLIST is `<me> <channel> q <mask> ...`; the standard
  // ban/exception numerics place the mask directly after the channel.
  const mask = event.code === 728 ? event.args[3] : event.args[2];
  if (listMode && channel && mask) {
    updateList(serverBufferKey(event.serverId, channel), listMode, mask, true);
  }
}

interface CentralState {
  target: { serverId: string; channel: string } | null;
  open: (serverId: string, channel: string) => void;
  close: () => void;
}

export const useChannelCentral = create<CentralState>((setState) => ({
  target: null,
  open: (serverId, channel) => setState({ target: { serverId, channel } }),
  close: () => setState({ target: null }),
}));
