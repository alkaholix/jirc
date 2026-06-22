import { create } from "zustand";
import { IrcEvent } from "../lib/api";
import { bufferKey } from "./store";

export interface ChanModes {
  /** Active boolean flag modes (e.g. m, t, i, n, s, p). */
  flags: Set<string>;
  /** Channel key (+k), or "" if none. */
  key: string;
  /** User limit (+l), or "" if none. */
  limit: string;
}

interface ModeState {
  byBuffer: Record<string, ChanModes>;
}

/** Tracks the current modes of channels, fed from MODE events + RPL_CHANNELMODEIS
 *  (324). Powers the Channel Central dialog. */
export const useChannelModes = create<ModeState>(() => ({ byBuffer: {} }));

// Modes whose parameter we surface, vs nick/list modes whose param we skip.
// `b` (ban) is tracked separately in the ban store below.
const NICK_MODES = "ovhaq";
const LIST_MODES = "eI";

/** Per-channel ban list (+b masks), fed from MODE +b/-b and RPL_BANLIST (367).
 *  Read by the Channel Central bans view (and a future isban, once the engine
 *  reads it). */
interface BanState {
  byBuffer: Record<string, string[]>;
}
export const useChannelBans = create<BanState>(() => ({ byBuffer: {} }));

/** The current +b masks for a channel. */
export function channelBans(serverId: string, channel: string): string[] {
  return useChannelBans.getState().byBuffer[bufferKey(serverId, channel)] ?? [];
}

function addBan(key: string, mask: string): void {
  useChannelBans.setState((s) => {
    const cur = s.byBuffer[key] ?? [];
    if (cur.some((m) => m.toLowerCase() === mask.toLowerCase())) return s;
    return { byBuffer: { ...s.byBuffer, [key]: [...cur, mask] } };
  });
}

function removeBan(key: string, mask: string): void {
  useChannelBans.setState((s) => {
    const cur = s.byBuffer[key];
    if (!cur) return s;
    return { byBuffer: { ...s.byBuffer, [key]: cur.filter((m) => m.toLowerCase() !== mask.toLowerCase()) } };
  });
}

function empty(): ChanModes {
  return { flags: new Set(), key: "", limit: "" };
}

/** Applies a mode change string (e.g. "+mt-i" / "+kl secret 50") to a mode set,
 *  also returning the +b/-b mask changes it carried (tracked separately). */
function applyDelta(
  cur: ChanModes,
  modeStr: string,
  params: string[]
): { modes: ChanModes; banOps: { sign: string; mask: string }[] } {
  const flags = new Set(cur.flags);
  let key = cur.key;
  let limit = cur.limit;
  let sign = "+";
  let pi = 0;
  const banOps: { sign: string; mask: string }[] = [];
  for (const ch of modeStr) {
    if (ch === "+" || ch === "-") {
      sign = ch;
    } else if (ch === "k") {
      if (sign === "+") key = params[pi++] ?? "";
      else {
        key = "";
        pi += 1; // -k consumes the (old) key param
      }
    } else if (ch === "l") {
      if (sign === "+") limit = params[pi++] ?? "";
      else limit = "";
    } else if (ch === "b") {
      const mask = params[pi++] ?? "";
      if (mask) banOps.push({ sign, mask });
    } else if (NICK_MODES.includes(ch) || LIST_MODES.includes(ch)) {
      pi += 1; // nick / +e/+I list-mode param we don't surface here
    } else if (sign === "+") {
      flags.add(ch);
    } else {
      flags.delete(ch);
    }
  }
  return { modes: { flags, key, limit }, banOps };
}

function set(key: string, modes: ChanModes): void {
  useChannelModes.setState((s) => ({ byBuffer: { ...s.byBuffer, [key]: modes } }));
}

/** Routes MODE / numeric (324 channel-modes, 367 ban-list) events into the
 *  channel-mode + ban trackers. */
export function routeModeEvent(ev: IrcEvent): void {
  if (ev.type === "mode" && ev.target.startsWith("#")) {
    const [modeStr, ...params] = ev.modes.split(" ");
    const k = bufferKey(ev.serverId, ev.target);
    const { modes, banOps } = applyDelta(useChannelModes.getState().byBuffer[k] ?? empty(), modeStr, params);
    set(k, modes);
    for (const op of banOps) (op.sign === "+" ? addBan : removeBan)(k, op.mask);
  } else if (ev.type === "numeric" && ev.code === 324) {
    // RPL_CHANNELMODEIS: args = [me, channel, +modes, param...]
    const [, channel, modeStr = "", ...params] = ev.args;
    if (channel) set(bufferKey(ev.serverId, channel), applyDelta(empty(), modeStr, params).modes);
  } else if (ev.type === "numeric" && ev.code === 367) {
    // RPL_BANLIST: args = [me, channel, banmask, setter?, time?]
    const [, channel, mask] = ev.args;
    if (channel && mask) addBan(bufferKey(ev.serverId, channel), mask);
  }
}

interface CentralState {
  target: { serverId: string; channel: string } | null;
  open: (serverId: string, channel: string) => void;
  close: () => void;
}

/** Open-state for the Channel Central dialog (triggered from the topic bar or
 *  the /channel command). */
export const useChannelCentral = create<CentralState>((setState) => ({
  target: null,
  open: (serverId, channel) => setState({ target: { serverId, channel } }),
  close: () => setState({ target: null }),
}));
