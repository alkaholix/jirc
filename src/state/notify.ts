import { create } from "zustand";
import { api, IrcEvent } from "../lib/api";
import { useSettings } from "./settings";
import { useStore } from "./store";
import { notify } from "../lib/notify";

/// Which watched nicks are currently online, per server (from ISON / RPL_ISON).
interface NotifyState {
  online: Record<string, string[]>;
}

export const useNotify = create<NotifyState>(() => ({ online: {} }));

/** Handles RPL_ISON (303) replies: diffs against the last poll and alerts on
 *  watched nicks coming online / going offline. */
export function routeNotifyEvent(ev: IrcEvent) {
  if (ev.type !== "numeric" || ev.code !== 303) return;
  const list = useSettings.getState().notifyList ?? [];
  if (!list.length) return;

  const reported = (ev.args[ev.args.length - 1] ?? "").split(/\s+/).filter(Boolean);
  const reportedLc = reported.map((n) => n.toLowerCase());
  const prev = useNotify.getState().online[ev.serverId] ?? [];
  const prevLc = prev.map((n) => n.toLowerCase());
  const notifications = useSettings.getState().notifications;

  for (const n of reported) {
    if (!prevLc.includes(n.toLowerCase()) && notifications) notify("Online", `${n} is online`);
  }
  for (const n of prev) {
    if (!reportedLc.includes(n.toLowerCase()) && notifications) notify("Offline", `${n} went offline`);
  }
  useNotify.setState((s) => ({ online: { ...s.online, [ev.serverId]: reported } }));
}

/** Polls each connected server with ISON for the watch list. */
export function pollNotify() {
  const list = useSettings.getState().notifyList ?? [];
  if (!list.length) return;
  const servers = useStore.getState().servers;
  for (const sid of Object.keys(servers)) {
    if (servers[sid].connected) api.sendRaw(sid, `ISON ${list.join(" ")}`).catch(() => {});
  }
}

/** All watched nicks currently online across servers (unique, for display). */
export function onlineFriends(): string[] {
  const all = Object.values(useNotify.getState().online).flat();
  return [...new Set(all)].sort();
}
