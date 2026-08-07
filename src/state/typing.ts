import { create } from "zustand";
import { api, IrcEvent } from "../lib/api";
import { bufferKey, useStore } from "./store";
import { useSettings } from "./settings";

/** How long a typing notification stands before it is assumed stale. A client
 *  that drops mid-compose never sends `done`, so without an expiry the
 *  indicator would stick forever. */
const TYPING_TTL_MS = 6000;

/** Minimum gap between outgoing `active` notifications, comfortably inside the
 *  receiver's TTL so the indicator never flickers while someone keeps typing. */
const REFRESH_MS = 3000;

interface Typist {
  nick: string;
  expires: number;
}

interface TypingState {
  /** Typists per buffer key. */
  byBuffer: Record<string, Typist[]>;
}

export const useTyping = create<TypingState>(() => ({ byBuffer: {} }));

const keyOf = (serverId: string, target: string) =>
  bufferKey(serverId, target, useStore.getState().servers[serverId]?.caseMapping ?? "rfc1459");

/** Drops a typist once their notification goes stale.
 *
 *  One timer per typist rather than a shared sweeper: there is no long-lived
 *  singleton to leak, expiry is exact instead of up to a tick late, and the
 *  timer is replaced whenever a fresh notification arrives. */
function scheduleExpiry(key: string, nick: string) {
  setTimeout(() => {
    const now = Date.now();
    useTyping.setState((s) => {
      const list = s.byBuffer[key];
      if (!list) return s;
      const live = list.filter((t) => t.nick !== nick || t.expires > now);
      if (live.length === list.length) return s;
      const next = { ...s.byBuffer };
      if (live.length) next[key] = live;
      else delete next[key];
      return { byBuffer: next };
    });
  }, TYPING_TTL_MS + 50);
}

/** Routes an incoming `+typing` notification into the per-buffer typist list.
 *
 *  Also watches the events that implicitly end a compose. This lives here, not
 *  in the store, so that all typing state stays in one module — the store
 *  importing this file would close an import cycle, since this file needs the
 *  store for buffer keys. */
export function routeTypingEvent(ev: IrcEvent) {
  // A message supersedes any pending notification from its sender. Without
  // this, a client that sends a PRIVMSG and no `+typing=done` would leave
  // "X is typing..." sitting under X's own message.
  if (ev.type === "message" && ev.from) {
    clearTyping(ev.serverId, ev.target, ev.from);
    // A direct message lands in the sender's query buffer, keyed by nick.
    clearTyping(ev.serverId, ev.from, ev.from);
    return;
  }
  if (ev.type === "part") {
    clearTyping(ev.serverId, ev.channel, ev.nick);
    return;
  }
  if (ev.type === "quit") {
    for (const channel of ev.channels) clearTyping(ev.serverId, channel, ev.nick);
    return;
  }
  if (ev.type !== "typing") return;
  if (!useSettings.getState().showTyping) return;
  const key = keyOf(ev.serverId, ev.target);
  useTyping.setState((s) => {
    const rest = (s.byBuffer[key] ?? []).filter((t) => t.nick !== ev.nick);
    const next = { ...s.byBuffer };
    // `paused` still displays: they are composing, just not this instant.
    // Only `done` removes them.
    const list =
      ev.state === "done"
        ? rest
        : [...rest, { nick: ev.nick, expires: Date.now() + TYPING_TTL_MS }];
    if (list.length) next[key] = list;
    else delete next[key];
    return { byBuffer: next };
  });
  if (ev.state !== "done") scheduleExpiry(key, ev.nick);
}

/** Clears typists for a buffer — one nick, or all of them. Called when someone
 *  actually speaks (the message supersedes the notification) or leaves. */
export function clearTyping(serverId: string, target: string, nick?: string) {
  const key = keyOf(serverId, target);
  useTyping.setState((s) => {
    const current = s.byBuffer[key];
    if (!current) return s;
    const list = nick ? current.filter((t) => t.nick !== nick) : [];
    const next = { ...s.byBuffer };
    if (list.length) next[key] = list;
    else delete next[key];
    return { byBuffer: next };
  });
}

/** "Bob is typing…" · "Bob and Sue are typing…" · "4 people are typing…" */
export function typingLabel(names: string[]): string {
  if (!names.length) return "";
  if (names.length === 1) return `${names[0]} is typing…`;
  if (names.length === 2) return `${names[0]} and ${names[1]} are typing…`;
  if (names.length === 3) return `${names[0]}, ${names[1]} and ${names[2]} are typing…`;
  return `${names.length} people are typing…`;
}

// ---- outgoing ----

/** Last state sent per buffer, so we neither spam the server nor fall silent
 *  while someone is still composing. */
const sent: Record<string, number> = {};

/** Announces that we are composing in `target`. Safe to call on every
 *  keystroke: it sends at most one TAGMSG per `REFRESH_MS`.
 *
 *  Whether the server can actually carry this is decided in the backend, which
 *  drops the notification unless `message-tags` was negotiated. */
export function sendTyping(serverId: string, target: string, composing: boolean) {
  if (!useSettings.getState().sendTyping) return;
  if (!useStore.getState().servers[serverId]?.connected) return;
  const key = keyOf(serverId, target);
  const now = Date.now();

  if (!composing) {
    if (sent[key] === undefined) return;
    delete sent[key];
    api.sendTyping(serverId, target, "done").catch(() => {});
    return;
  }
  if (sent[key] !== undefined && now - sent[key] < REFRESH_MS) return;
  sent[key] = now;
  api.sendTyping(serverId, target, "active").catch(() => {});
}

/** Called once a message is actually sent: the compose is over. Sending the
 *  message ends the typing state implicitly, so no `done` is needed — clearing
 *  the record just lets the next keystroke start a fresh notification. */
export function typingSent(serverId: string, target: string) {
  delete sent[keyOf(serverId, target)];
}
