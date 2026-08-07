import { create } from "zustand";
import { api, IrcEvent } from "../lib/api";
import { useSettings } from "./settings";
import { useStore } from "./store";
import { notify } from "../lib/notify";
import { playAlertSound } from "../lib/sound";

/// Which watched nicks are currently online, per server.
///
/// Two sources feed this. Where the server advertises MONITOR the state is
/// pushed (RPL_MONONLINE/RPL_MONOFFLINE) and arrives as deltas; elsewhere it is
/// polled with ISON, whose reply is the complete online set. `applyOnline`
/// handles both by taking the delta form, with the poll passing the difference.
interface NotifyState {
  online: Record<string, string[]>;
  /// Servers whose MONITOR list we have already registered, so a reconnect
  /// re-sends it but a second ISUPPORT line does not.
  monitored: Record<string, string>;
}

export const useNotify = create<NotifyState>(() => ({ online: {}, monitored: {} }));

/** Whether this server pushes notify state rather than needing a poll. */
function usesMonitor(serverId: string): boolean {
  return !!useStore.getState().servers[serverId]?.monitor;
}

/** Announces arrivals and departures, then records the new online set. */
function applyOnline(serverId: string, nowOnline: string[], wentOffline: string[]) {
  const notifications = useSettings.getState().notifications;
  const network = useStore.getState().servers[serverId]?.name ?? "";
  const prev = useNotify.getState().online[serverId] ?? [];
  const prevLc = prev.map((n) => n.toLowerCase());

  for (const n of nowOnline) {
    if (prevLc.includes(n.toLowerCase())) continue;
    // Fire `on NOTIFY` regardless of the desktop-notification setting.
    api.scriptNotify(serverId, network, n, true).catch(() => {});
    if (notifications) notify("Online", `${n} is online`);
    playAlertSound("online");
  }
  for (const n of wentOffline) {
    if (!prevLc.includes(n.toLowerCase())) continue;
    api.scriptNotify(serverId, network, n, false).catch(() => {});
    if (notifications) notify("Offline", `${n} went offline`);
  }

  const offlineLc = wentOffline.map((n) => n.toLowerCase());
  const next = [
    ...prev.filter((n) => !offlineLc.includes(n.toLowerCase())),
    ...nowOnline.filter((n) => !prevLc.includes(n.toLowerCase())),
  ];
  useNotify.setState((s) => ({ online: { ...s.online, [serverId]: next } }));
}

/** Nicks from a MONITOR numeric: a comma-separated list of `nick[!user@host]`. */
function monitorTargets(args: string[]): string[] {
  return (args[args.length - 1] ?? "")
    .split(",")
    .map((t) => t.trim().split("!")[0])
    .filter(Boolean);
}

/** Handles ISON (303) and MONITOR (730/731/732/734) replies. */
export function routeNotifyEvent(ev: IrcEvent) {
  // Drop a dropped server's state so a reconnect re-registers MONITOR and
  // re-announces who is online, rather than treating them as already known.
  if (ev.type === "disconnected") {
    resetNotify(ev.serverId);
    return;
  }
  if (ev.type !== "numeric") return;
  const list = useSettings.getState().notifyList ?? [];

  switch (ev.code) {
    // RPL_ISON — the full online set, so anything absent is now offline.
    case 303: {
      if (!list.length) return;
      const reported = (ev.args[ev.args.length - 1] ?? "").split(/\s+/).filter(Boolean);
      const reportedLc = reported.map((n) => n.toLowerCase());
      const prev = useNotify.getState().online[ev.serverId] ?? [];
      applyOnline(
        ev.serverId,
        reported,
        prev.filter((n) => !reportedLc.includes(n.toLowerCase()))
      );
      return;
    }
    // RPL_MONONLINE / RPL_MONOFFLINE — pushed deltas.
    case 730:
      applyOnline(ev.serverId, monitorTargets(ev.args), []);
      return;
    case 731:
      applyOnline(ev.serverId, [], monitorTargets(ev.args));
      return;
    // RPL_MONLIST — the server's view of our list, on request. Informational.
    case 732:
      return;
    // ERR_MONLISTFULL — the rest of the list was rejected, so those nicks will
    // never be reported. Fall back to polling for this server.
    case 734:
      useNotify.setState((s) => {
        const monitored = { ...s.monitored };
        delete monitored[ev.serverId];
        return { monitored };
      });
      return;
    default:
      return;
  }
}

/** Registers the watch list with a MONITOR-capable server, or re-registers it
 *  when the list changes. No-op for servers that need polling. */
function syncMonitor(serverId: string) {
  const list = useSettings.getState().notifyList ?? [];
  const wanted = list.join(",");
  const current = useNotify.getState().monitored[serverId];
  if (current === wanted) return;

  // MONITOR C clears the list; re-adding is simpler and less error-prone than
  // diffing, and the list is small enough that it costs nothing.
  api.sendRaw(serverId, "MONITOR C").catch(() => {});
  if (wanted) api.sendRaw(serverId, `MONITOR + ${wanted}`).catch(() => {});
  useNotify.setState((s) => ({ monitored: { ...s.monitored, [serverId]: wanted } }));
  // A cleared list means nobody is online by definition.
  if (!wanted) {
    useNotify.setState((s) => ({ online: { ...s.online, [serverId]: [] } }));
  }
}

/** Keeps every connected server's notify state current: MONITOR where it is
 *  supported, an ISON poll where it is not. */
export function pollNotify() {
  const list = useSettings.getState().notifyList ?? [];
  const servers = useStore.getState().servers;
  for (const sid of Object.keys(servers)) {
    if (!servers[sid].connected) continue;
    if (usesMonitor(sid)) {
      syncMonitor(sid);
      continue;
    }
    if (list.length) api.sendRaw(sid, `ISON ${list.join(" ")}`).catch(() => {});
  }
}

/** Forgets a disconnected server's state so a reconnect re-registers cleanly. */
export function resetNotify(serverId: string) {
  useNotify.setState((s) => {
    const online = { ...s.online };
    const monitored = { ...s.monitored };
    delete online[serverId];
    delete monitored[serverId];
    return { online, monitored };
  });
}

/** All watched nicks currently online across servers (unique, for display). */
export function onlineFriends(): string[] {
  const all = Object.values(useNotify.getState().online).flat();
  return [...new Set(all)].sort();
}
