import { create } from "zustand";
import { IrcEvent } from "../lib/api";

/// Your own away state per server (from RPL_NOWAWAY / RPL_UNAWAY).
interface AwayState {
  away: Record<string, boolean>;
}

export const useAway = create<AwayState>(() => ({ away: {} }));

/** Routes self-away events into the store. */
export function routeAwayEvent(ev: IrcEvent) {
  if (ev.type !== "selfAway") return;
  useAway.setState((s) => ({ away: { ...s.away, [ev.serverId]: ev.away } }));
}
