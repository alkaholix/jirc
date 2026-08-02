import { create } from "zustand";
import { api, IrcEvent, Member } from "../lib/api";
import { useSettings } from "./settings";
import { notify } from "../lib/notify";
import { stripFormatting } from "../ircFormat/parse";
import { ircxDisplay } from "../lib/ircx";
import { dccOffers } from "./dcc";
import { useTransfers } from "./transfers";
import { playAlertSound } from "../lib/sound";

export type BufferKind = "status" | "channel" | "query" | "window";

export type LineKind =
  | "msg"
  | "notice"
  | "action"
  | "whisper"
  | "event"
  | "error"
  | "system"
  | "separator";

export interface Line {
  id: number;
  ts: number;
  kind: LineKind;
  from?: string;
  text: string;
  self?: boolean;
  /** For an outgoing whisper: who it was whispered to. */
  to?: string;
}

export interface Buffer {
  key: string;
  serverId: string;
  name: string; // channel/nick, or STATUS for the server console
  kind: BufferKind;
  lines: Line[];
  members: Member[];
  topic?: string;
  unread: number;
  mention: boolean;
  /** Whether new chat lines are appended to this buffer's disk log. */
  logging?: boolean;
  /** For a custom `@window` (kind "window"): its display kind (listbox/text/…). */
  windowKind?: string;
  /** Script-controlled display title; the stable buffer name remains unchanged. */
  windowTitle?: string;
  /** One-based selected rows in a custom listbox window. */
  windowSelected?: number[];
  /** Retained canvas operations for a custom picture window. */
  windowDrawing?: Array<{ op: string; args: string[] }>;
}

export type IrcCaseMapping = "ascii" | "rfc1459" | "strict-rfc1459";

export interface Server {
  id: string;
  name: string;
  nick: string;
  connected: boolean;
  registered: boolean;
  chanTypes: string;
  prefixes: string;
  prefixModes: string;
  caseMapping: IrcCaseMapping;
  statusMsg: string;
  chanModes: string;
  modesPerLine: number;
}

export interface ChannelListEntry {
  channel: string;
  users: number;
  topic: string;
}

export interface ChannelListWindow {
  serverId: string;
  entries: ChannelListEntry[];
  loading: boolean;
}

const MAX_LIST_ENTRIES = 20000;

export const STATUS = "(status)";
const MAX_LINES = 2000;

/** Canonical IRC name key. RFC1459 additionally equates `[]\\^` with `{}|~`;
 * strict-rfc1459 excludes the `^`/`~` pair, while ascii folds only A-Z. */
export function ircCasefold(name: string, mapping: IrcCaseMapping = "rfc1459"): string {
  let out = "";
  for (const ch of name) {
    if (ch >= "A" && ch <= "Z") out += ch.toLowerCase();
    else if (mapping !== "ascii" && ch === "[") out += "{";
    else if (mapping !== "ascii" && ch === "]") out += "}";
    else if (mapping !== "ascii" && ch === "\\") out += "|";
    else if (mapping === "rfc1459" && ch === "^") out += "~";
    else out += ch;
  }
  return out;
}

export const bufferKey = (
  serverId: string,
  name: string,
  mapping: IrcCaseMapping = "rfc1459"
) => `${serverId}\u0000${ircCasefold(name, mapping)}`;

let lineSeq = 1;
const nextId = () => lineSeq++;

/** Channels we were in when a connection dropped, per server id — so
 *  "rejoin on disconnect" can rejoin them even if their windows were closed. */
const rejoinPending = new Map<string, string[]>();

const DEFAULT_PREFIXES = "~&@%+";

/** Ranks a member by their highest prefix using the server's advertised order
 *  (from ISUPPORT PREFIX, e.g. "~&@%+" on IRC or ".@+" on an IRCX server). */
function sortMembers(
  members: Member[],
  prefixes: string,
  mapping: IrcCaseMapping = "rfc1459"
): Member[] {
  const rank = (m: Member) => {
    if (!m.prefix) return prefixes.length;
    const i = prefixes.indexOf(m.prefix[0]);
    return i < 0 ? prefixes.length : i;
  };
  return [...members].sort((a, b) => {
    const d = rank(a) - rank(b);
    return d !== 0 ? d : ircCasefold(a.nick, mapping).localeCompare(ircCasefold(b.nick, mapping));
  });
}

const DEFAULT_CHANTYPES = "#&!+%";
// Driven by the server's advertised CHANTYPES (IRCX servers list their '%#'/'%&'
// prefixes there, e.g. CHANTYPES=%#). The default above covers the pre-ISUPPORT
// window.
const isChannelFor = (name: string, chanTypes: string) =>
  name.length > 0 && chanTypes.includes(name[0]);

/** Resolves a normal channel or STATUSMSG target to its bare channel. */
function channelTargetFor(name: string, chanTypes: string, statusMsg: string): string | null {
  let bare = name;
  while (bare && statusMsg.includes(bare[0])) bare = bare.slice(1);
  if (bare !== name && isChannelFor(bare, chanTypes)) return bare;
  return isChannelFor(name, chanTypes) ? name : null;
}

/** Matches a wildcard mask (`*`/`?`) against text, case-insensitively. */
function wildcardMatch(mask: string, text: string): boolean {
  const re = new RegExp(
    "^" + mask.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*").replace(/\?/g, ".") + "$",
    "i"
  );
  return re.test(text);
}

/** Whether `nick` is covered by any ignore entry (nick or `nick!user@host` mask). */
function isIgnored(nick: string): boolean {
  const { ignores } = useSettings.getState();
  return ignores.some((entry) => {
    const nickPart = entry.split("!")[0] || entry;
    return wildcardMatch(entry, nick) || wildcardMatch(nickPart, nick);
  });
}

interface State {
  servers: Record<string, Server>;
  buffers: Record<string, Buffer>;
  order: string[];
  active: string | null;
  channelList: ChannelListWindow | null;
  /** Buffers currently popped out into their own OS window (keyed by buffer key). */
  poppedOut: Record<string, boolean>;

  openChannelList: (serverId: string) => void;
  closeChannelList: () => void;
  ensureServer: (serverId: string, name: string) => void;
  ensureBuffer: (serverId: string, name: string, kind: BufferKind) => string;
  closeBuffer: (key: string) => void;
  renameBuffer: (key: string, newName: string) => void;
  closeServer: (serverId: string) => void;
  setActive: (key: string) => void;
  appendLine: (
    serverId: string,
    name: string,
    kind: BufferKind,
    line: Omit<Line, "id" | "ts">,
    atMs?: number
  ) => void;
  handleEvent: (ev: IrcEvent) => void;
  setPoppedOut: (key: string, val: boolean) => void;
  addDetachedBuffer: (server: Server, buffer: Buffer) => void;
}

export const useStore = create<State>((set, get) => {
  const mappingFor = (serverId: string): IrcCaseMapping =>
    get().servers[serverId]?.caseMapping ?? "rfc1459";
  const keyFor = (serverId: string, name: string): string =>
    bufferKey(serverId, name, mappingFor(serverId));
  const namesEqual = (serverId: string, left: string, right: string): boolean =>
    ircCasefold(left, mappingFor(serverId)) === ircCasefold(right, mappingFor(serverId));

  const patchBuffer = (key: string, fn: (b: Buffer) => Buffer) =>
    set((s) => (s.buffers[key] ? { buffers: { ...s.buffers, [key]: fn(s.buffers[key]) } } : s));

  const ensureBuffer = (serverId: string, name: string, kind: BufferKind): string => {
    const key = keyFor(serverId, name);
    if (!get().buffers[key]) {
      const buf: Buffer = {
        key,
        serverId,
        name,
        kind,
        lines: [],
        members: [],
        unread: 0,
        mention: false,
      };
      set((s) => ({ buffers: { ...s.buffers, [key]: buf }, order: [...s.order, key] }));
      // Give the new window a $wid in the engine (status reads as "Status Window").
      api.scriptWindowOpen(serverId, kind === "status" ? "Status Window" : name).catch(() => {});
      // Load any prior log for this buffer.
      const srv = get().servers[serverId];
      if (srv && kind !== "status") {
        api
          .logRead(srv.name, name)
          .then((text) => {
            if (!text) return;
            const recent = text.trimEnd().split("\n").slice(-200);
            patchBuffer(key, (b) =>
              b.lines.length > 0
                ? b
                : {
                    ...b,
                    lines: recent.map((t) => ({
                      id: nextId(),
                      ts: 0,
                      kind: "system" as LineKind,
                      text: t,
                    })),
                  }
            );
          })
          .catch(() => {});
      }
    }
    return key;
  };

  const appendLine: State["appendLine"] = (serverId, name, kind, line, atMs) => {
    const key = ensureBuffer(serverId, name, kind);
    const srv = get().servers[serverId];
    const full: Line = { ...line, id: nextId(), ts: atMs && !Number.isNaN(atMs) ? atMs : Date.now() };
    const isActive = get().active === key;
    const settings = useSettings.getState();

    const words = srv ? [srv.nick, ...settings.highlightWords] : settings.highlightWords;
    const mentioned =
      !!line.from &&
      !line.self &&
      words.some((w) => {
        if (!w) return false;
        // Never let a bad word/nick break message rendering.
        try {
          return new RegExp(`\\b${escapeRegex(w)}\\b`, "i").test(line.text);
        } catch {
          return false;
        }
      });

    const isAlert = line.kind === "msg" || line.kind === "action" || line.kind === "whisper";

    patchBuffer(key, (b) => ({
      ...b,
      lines: [...b.lines.slice(-(MAX_LINES - 1)), full],
      unread: isActive ? 0 : b.unread + (isAlert ? 1 : 0),
      mention: b.mention || (!isActive && mentioned),
    }));

    // Desktop notification for mentions and private messages.
    if (isAlert && line.kind !== "whisper" && !line.self) {
      const windowFocused = typeof document !== "undefined" && document.hasFocus();
      if ((mentioned || kind === "query") && (!isActive || !windowFocused)) {
        const where = kind === "query" ? (line.from ?? name) : name;
        if (settings.notifications) {
          notify(where, `${line.from ? `${line.from}: ` : ""}${stripFormatting(line.text)}`);
        }
        playAlertSound(kind === "query" ? "private" : "mention");
      }
    }

    // Persist to the on-disk log (best effort).
    if (srv && line.kind !== "system" && get().buffers[key]?.logging !== false) {
      const ts = new Date(full.ts).toISOString().slice(11, 19);
      const prefix = line.from ? `<${line.from}> ` : "";
      api.logAppend(srv.name, name, `[${ts}] ${prefix}${line.text}`).catch(() => {});
    }
  };

  const handleEvent: State["handleEvent"] = (ev) => {
    // `scriptServer` isn't a per-server event — App.tsx opens the window and
    // starts the connection. Ignore it here (and narrow the union so the rest
    // can rely on `serverId`).
    if (ev.type === "scriptServer") return;
    const sid = ev.serverId;
    const settings = useSettings.getState();
    const chanTypes = get().servers[sid]?.chanTypes ?? DEFAULT_CHANTYPES;
    const statusMsg = get().servers[sid]?.statusMsg ?? "";
    const isChannel = (name: string) => isChannelFor(name, chanTypes);
    const channelTarget = (name: string) => channelTargetFor(name, chanTypes, statusMsg);
    const sys = (text: string, kind: LineKind = "system") =>
      appendLine(sid, STATUS, "status", { kind, text });

    switch (ev.type) {
      case "connected":
        sys("Connected.");
        break;
      case "registered": {
        const wasRegistered = !!get().servers[sid]?.registered;
        set((s) =>
          s.servers[sid]
            ? { servers: { ...s.servers, [sid]: { ...s.servers[sid], nick: ev.nick, registered: true, connected: true } } }
            : s
        );
        sys(`Registered as ${ev.nick}.`);
        // Rejoin channels after a disconnect (reconnect is when we can actually
        // JOIN). Rejoin the union of the channels we were in when we dropped and
        // any still-open channel windows, so it works whether or not those
        // windows were kept open.
        if (wasRegistered && settings.rejoinOnReconnect) {
          const open = Object.values(get().buffers)
            .filter((b) => b.serverId === sid && b.kind === "channel")
            .map((b) => b.name);
          const channels = new Set([...(rejoinPending.get(sid) ?? []), ...open]);
          for (const ch of channels) api.join(sid, ch).catch(() => {});
        }
        rejoinPending.delete(sid);
        break;
      }
      case "disconnected": {
        set((s) =>
          s.servers[sid] ? { servers: { ...s.servers, [sid]: { ...s.servers[sid], connected: false } } } : s
        );
        sys(`Disconnected: ${ev.reason}`, "error");
        // Remember the channels we were in, so "rejoin on disconnect" can restore
        // them on reconnect even if we close their windows just below.
        const chans = Object.values(get().buffers)
          .filter((b) => b.serverId === sid && b.kind === "channel")
          .map((b) => b.name);
        if (chans.length) rejoinPending.set(sid, chans);
        else rejoinPending.delete(sid);
        // Optionally close channel windows on disconnect.
        if (!settings.keepOpenOnKickQuit) {
          set((s) => {
            const buffers = { ...s.buffers };
            const order: string[] = [];
            for (const key of s.order) {
              const b = buffers[key];
              if (b.serverId === sid && b.kind === "channel") delete buffers[key];
              else order.push(key);
            }
            const active = s.active && buffers[s.active] ? s.active : order[order.length - 1] ?? null;
            return { buffers, order, active };
          });
        }
        break;
      }
      case "message": {
        const srv = get().servers[sid];
        const self = !!srv && !!ev.from && namesEqual(sid, ev.from, srv.nick);
        if (ev.from && !self && isIgnored(ev.from)) break;
        const action = parseAction(ev.text);
        const atMs = ev.time ? Date.parse(ev.time) : undefined;
        const channel = channelTarget(ev.target);
        if (channel) {
          appendLine(
            sid,
            channel,
            "channel",
            {
              kind: ev.kind === "notice" ? "notice" : action ? "action" : "msg",
              from: ev.from ?? undefined,
              text: action ?? ev.text,
              self,
            },
            atMs
          );
        } else {
          // Direct message/notice: route to a query with the other party.
          const who = self ? ev.target : ev.from ?? "(server)";
          if (ev.kind === "notice" && !ev.from) {
            sys(`-${ev.target}- ${ev.text}`, "notice");
          } else {
            appendLine(
              sid,
              who,
              "query",
              {
                kind: ev.kind === "notice" ? "notice" : action ? "action" : "msg",
                from: ev.from ?? undefined,
                text: action ?? ev.text,
                self,
              },
              atMs
            );
          }
        }
        break;
      }
      case "join": {
        const srv = get().servers[sid];
        const self = !!srv && namesEqual(sid, ev.nick, srv.nick);
        ensureBuffer(sid, ev.channel, "channel");
        const key = keyFor(sid, ev.channel);
        if (self) get().setActive(key);
        patchBuffer(key, (b) =>
          b.members.some((m) => namesEqual(sid, m.nick, ev.nick))
            ? b
            : {
                ...b,
                members: sortMembers(
                  [...b.members, { nick: ev.nick, prefix: "" }],
                  srv?.prefixes ?? DEFAULT_PREFIXES,
                  mappingFor(sid)
                ),
              }
        );
        appendLine(sid, ev.channel, "channel", { kind: "event", text: `→ ${ev.nick} joined ${ircxDisplay(ev.channel)}` });
        break;
      }
      case "part":
        removeMember(sid, ev.channel, ev.nick);
        appendLine(sid, ev.channel, "channel", {
          kind: "event",
          text: `← ${ev.nick} left ${ircxDisplay(ev.channel)}${ev.reason ? ` (${ev.reason})` : ""}`,
        });
        break;
      case "quit":
        for (const channel of ev.channels) {
          removeMember(sid, channel, ev.nick);
          appendLine(sid, channel, "channel", {
            kind: "event",
            text: `← ${ev.nick} quit${ev.reason ? ` (${ev.reason})` : ""}`,
          });
        }
        break;
      case "kick": {
        removeMember(sid, ev.channel, ev.nick);
        const who = ev.isSelf ? "You were" : `${ev.nick} was`;
        appendLine(sid, ev.channel, "channel", {
          kind: ev.isSelf ? "error" : "event",
          text: `${who} kicked from ${ircxDisplay(ev.channel)}${ev.by ? ` by ${ev.by}` : ""}${
            ev.reason ? ` (${ev.reason})` : ""
          }`,
        });
        if (ev.isSelf) {
          if (settings.rejoinOnKick) api.join(sid, ev.channel).catch(() => {});
          else if (!settings.keepOpenOnKickQuit) get().closeBuffer(keyFor(sid, ev.channel));
        }
        break;
      }
      // ---- Script-driven custom windows (@window) ----
      case "windowOpen": {
        const key = keyFor(sid, ev.name);
        const isNew = !get().buffers[key];
        ensureBuffer(sid, ev.name, "window");
        patchBuffer(key, (b) => (b.windowKind === ev.kind ? b : { ...b, windowKind: ev.kind }));
        // Surface a newly-created window (mIRC pops it up); don't steal focus if a
        // script re-issues /window on one that already exists.
        if (isNew) get().setActive(key);
        break;
      }
      case "windowClose":
        get().closeBuffer(keyFor(sid, ev.name));
        break;
      case "windowLine": {
        // Mirror the backend WindowStore ops (1-based positions) on the buffer's
        // lines. Window lines are plain rows (no nick/timestamp/logging).
        const key = ensureBuffer(sid, ev.name, "window");
        const mk = (text: string): Line => ({ id: nextId(), ts: 0, kind: "system", text });
        patchBuffer(key, (b) => {
          const lines = [...b.lines];
          switch (ev.op) {
            case "add":
              lines.push(mk(ev.text));
              break;
            case "insert": {
              const idx = Math.min(Math.max(ev.n - 1, 0), lines.length);
              lines.splice(idx, 0, mk(ev.text));
              return {
                ...b,
                lines,
                windowSelected: (b.windowSelected ?? []).map((line) =>
                  line >= idx + 1 ? line + 1 : line
                ),
              };
            }
            case "replace": {
              const i = ev.n - 1;
              if (i >= 0 && i < lines.length) lines[i] = mk(ev.text);
              break;
            }
            case "delete": {
              const i = ev.n - 1;
              if (i >= 0 && i < lines.length) {
                lines.splice(i, 1);
                const selected = (b.windowSelected ?? [])
                  .filter((line) => line !== ev.n)
                  .map((line) => (line > ev.n ? line - 1 : line));
                return { ...b, lines, windowSelected: selected };
              }
              break;
            }
            case "clear":
              return { ...b, lines: [], windowSelected: [], windowDrawing: [] };
            case "select":
              return {
                ...b,
                windowSelected: ev.n > 0 && ev.n <= lines.length ? [ev.n] : [],
              };
            case "selectAdd":
              return {
                ...b,
                windowSelected:
                  ev.n > 0 && ev.n <= lines.length
                    ? [...new Set([...(b.windowSelected ?? []), ev.n])].sort((a, c) => a - c)
                    : b.windowSelected,
              };
            case "deselect":
              return {
                ...b,
                windowSelected: (b.windowSelected ?? []).filter((line) => line !== ev.n),
              };
          }
          return { ...b, lines };
        });
        break;
      }
      case "windowTitle": {
        const key = ensureBuffer(sid, ev.name, "window");
        patchBuffer(key, (buffer) => ({ ...buffer, windowTitle: ev.title }));
        break;
      }
      case "windowDraw": {
        const key = ensureBuffer(sid, ev.name, "window");
        patchBuffer(key, (buffer) => ({
          ...buffer,
          windowDrawing:
            ev.op === "drawsize"
              ? [...(buffer.windowDrawing ?? []).filter((draw) => draw.op !== "drawsize"), { op: ev.op, args: ev.args }]
              : [...(buffer.windowDrawing ?? []), { op: ev.op, args: ev.args }],
        }));
        break;
      }
      case "awayChange":
        if (settings.showAway) {
          for (const channel of ev.channels) {
            appendLine(sid, channel, "channel", {
              kind: "event",
              text: ev.away
                ? `${ev.nick} is now away${ev.message ? ` (${ev.message})` : ""}`
                : `${ev.nick} is back`,
            });
          }
        }
        break;
      case "dccChatOpen": {
        ensureBuffer(sid, ev.id, "query");
        appendLine(sid, ev.id, "query", {
          kind: "event",
          text: ev.outgoing
            ? `DCC chat offered to ${ev.nick} — waiting for them to connect…`
            : `DCC chat with ${ev.nick} — connecting…`,
        });
        break;
      }
      case "dccChatLine":
        appendLine(sid, ev.id, "query", { kind: "msg", from: ev.from, text: ev.text });
        break;
      case "dccChatClosed":
        appendLine(sid, ev.id, "query", { kind: "event", text: "DCC chat closed." });
        break;
      case "dccChatOffer":
        // Remember the offer so `/dcc get <nick>` can connect; the approve/decline
        // prompt is shown once, by the main window (see App.tsx).
        dccOffers.set(sid, { nick: ev.nick, ip: ev.ip, port: ev.port, token: ev.token });
        break;
      case "dccFileOffer":
        dccOffers.setFile(sid, {
          nick: ev.nick,
          ip: ev.ip,
          port: ev.port,
          filename: ev.filename,
          size: ev.size,
          token: ev.token,
        });
        break;
      case "dccTransfer":
        useTransfers.getState().upsert({
          serverId: ev.serverId,
          id: ev.id,
          kind: ev.kind,
          nick: ev.nick,
          filename: ev.filename,
          transferred: ev.transferred,
          size: ev.size,
          status: ev.status,
        });
        break;
      case "raw": {
        if (settings.trace) {
          sys(`${ev.direction === "in" ? "<<" : ">>"} ${ev.line}`);
        } else if (settings.showPingPong && ev.direction === "in") {
          const toks = ev.line.split(" ");
          const cmd = toks[0].startsWith(":") ? toks[1] : toks[0];
          if (cmd && cmd.toUpperCase() === "PING") sys("Ping? Pong!");
        }
        break;
      }
      case "nickChange": {
        const srv = get().servers[sid];
        if (srv && namesEqual(sid, ev.old, srv.nick)) {
          set((s) => ({ servers: { ...s.servers, [sid]: { ...s.servers[sid], nick: ev.new } } }));
        }
        renameMember(sid, ev.old, ev.new);
        break;
      }
      case "names":
        ensureBuffer(sid, ev.channel, "channel");
        patchBuffer(keyFor(sid, ev.channel), (b) => ({
          ...b,
          members: sortMembers(
            ev.members,
            get().servers[sid]?.prefixes ?? DEFAULT_PREFIXES,
            mappingFor(sid)
          ),
        }));
        break;
      case "topic":
        ensureBuffer(sid, ev.channel, "channel");
        patchBuffer(keyFor(sid, ev.channel), (b) => ({ ...b, topic: ev.topic ?? undefined }));
        appendLine(sid, ev.channel, "channel", {
          kind: "event",
          text: `Topic${ev.setBy ? ` (set by ${ev.setBy})` : ""}: ${ev.topic ?? "(none)"}`,
        });
        break;
      case "mode": {
        const inChan = isChannel(ev.target);
        // Show who set it ("Snue sets mode: +v Bob"), not just the change.
        const text = ev.by
          ? `${ev.by} sets mode: ${ev.modes}`
          : `Mode ${ircxDisplay(ev.target)}: ${ev.modes}`;
        appendLine(sid, inChan ? ev.target : STATUS, inChan ? "channel" : "status", {
          kind: "event",
          text,
        });
        break;
      }
      case "ownerGranted": {
        // We just got +q: provision IRCX owner/host keys + access, then store them.
        const srv = get().servers[sid];
        const network = srv?.name ?? sid;
        api
          .ircxClaimOwner(sid, network, ev.channel)
          .then((keys) =>
            appendLine(sid, ev.channel, "channel", {
              kind: "system",
              text: `IRCX: claimed owner of ${ircxDisplay(ev.channel)} — OWNERKEY ${keys.ownerkey} · HOSTKEY ${keys.hostkey} (saved)`,
            })
          )
          .catch((e) =>
            appendLine(sid, ev.channel, "channel", {
              kind: "system",
              text: `IRCX owner setup failed: ${e}`,
            })
          );
        break;
      }
      case "ownerRevoked": {
        // Takeover protection: someone stripped our +q. Reclaim with the stored
        // OWNERKEY, clear the owner access list, kick the offender — the +q echo
        // for the reclaim then re-runs ownerGranted, which cuts fresh keys.
        const srv = get().servers[sid];
        const network = srv?.name ?? sid;
        api
          .ircxOwnerProtect(sid, network, ev.channel, ev.by)
          .then(() =>
            appendLine(sid, ev.channel, "channel", {
              kind: "system",
              text: `IRCX protection: ${ev.by} removed your owner — reclaiming, clearing owner access, kicking ${ev.by}`,
            })
          )
          .catch(() => {}); // no stored keys for this channel — nothing to protect
        break;
      }
      case "numeric": {
        const code = ev.code;
        const body = ev.args.slice(1).join(" ");
        const isMotd = code === 372 || code === 375 || code === 376 || code === 377;
        if (isMotd) {
          if (!settings.skipMotd) sys(body);
        } else if (code >= 400) {
          // Errors (nick in use, no such channel, …) always show.
          sys(`[${code}] ${body}`);
        } else if (settings.trace) {
          // Informational numerics (server info, lusers, …) only in trace mode.
          sys(`[${code}] ${body}`);
        }
        break;
      }
      case "error":
        sys(ev.message, "error");
        break;
      case "echo": {
        const k: BufferKind =
          ev.target === STATUS ? "status" : isChannel(ev.target) ? "channel" : "query";
        appendLine(sid, ev.target, k, { kind: "system", text: ev.text });
        break;
      }
      case "isupport":
        set((s) =>
          s.servers[sid]
            ? {
                servers: {
                  ...s.servers,
                  [sid]: {
                    ...s.servers[sid],
                    chanTypes: ev.chanTypes,
                    prefixes: ev.prefixes,
                    prefixModes: ev.prefixModes,
                    caseMapping: ev.caseMapping,
                    statusMsg: ev.statusMsg,
                    chanModes: ev.chanModes,
                    modesPerLine: ev.modesPerLine,
                  },
                },
              }
            : s
        );
        break;
      case "whois": {
        sys(`── WHOIS ${ev.nick} ──`);
        for (const line of ev.lines) sys(`  ${line}`);
        break;
      }
      case "invite": {
        const who = ev.from ?? "someone";
        sys(`→ ${who} invited you to ${ircxDisplay(ev.channel)}`);
        if (settings.notifications) notify("Invite", `${who} invited you to ${ircxDisplay(ev.channel)}`);
        playAlertSound("invite");
        break;
      }
      case "ircxState":
        sys(`IRCX enabled (version ${ev.version ?? "?"}, packages ${ev.packages ?? "-"}).`);
        break;
      case "ircxProp":
        sys(`prop ${ev.object} ${ev.name} = ${ev.value}`);
        break;
      case "ircxAccess":
        sys(`access ${ev.object}: ${ev.level ?? ""} ${ev.mask ?? ""}`.trim());
        break;
      case "listEntry":
        set((s) =>
          s.channelList && s.channelList.serverId === sid
            ? {
                channelList: {
                  ...s.channelList,
                  entries:
                    s.channelList.entries.length >= MAX_LIST_ENTRIES
                      ? s.channelList.entries
                      : [
                          ...s.channelList.entries,
                          { channel: ev.channel, users: ev.users, topic: ev.topic },
                        ],
                },
              }
            : s
        );
        break;
      case "listEnd":
        set((s) =>
          s.channelList && s.channelList.serverId === sid
            ? { channelList: { ...s.channelList, loading: false } }
            : s
        );
        break;
      case "whisper": {
        if (ev.from && isIgnored(ev.from)) break;
        appendLine(sid, ev.channel, "channel", {
          kind: "whisper",
          from: ev.from ?? undefined,
          text: ev.text,
        });
        // Whispers are private — notify (and flag the channel) like a mention.
        const wkey = keyFor(sid, ev.channel);
        if (get().active !== wkey) patchBuffer(wkey, (b) => ({ ...b, mention: true }));
        {
          const windowFocused = typeof document !== "undefined" && document.hasFocus();
          if (get().active !== wkey || !windowFocused) {
            if (settings.notifications) {
              notify(`Whisper from ${ev.from ?? "?"} in ${ev.channel}`, ev.text);
            }
            playAlertSound("private");
          }
        }
        break;
      }
      default:
        break;
    }
  };

  const removeMember = (sid: string, channel: string, nick: string) =>
    patchBuffer(keyFor(sid, channel), (b) => ({
      ...b,
      members: b.members.filter((m) => !namesEqual(sid, m.nick, nick)),
    }));

  const renameMember = (sid: string, oldNick: string, newNick: string) =>
    set((s) => {
      const prefixes = s.servers[sid]?.prefixes ?? DEFAULT_PREFIXES;
      const buffers = { ...s.buffers };
      for (const key of Object.keys(buffers)) {
        const b = buffers[key];
        if (b.serverId === sid && b.members.some((m) => namesEqual(sid, m.nick, oldNick))) {
          buffers[key] = {
            ...b,
            members: sortMembers(
              b.members.map((m) =>
                namesEqual(sid, m.nick, oldNick) ? { ...m, nick: newNick } : m
              ),
              prefixes,
              mappingFor(sid)
            ),
          };
        }
      }
      return { buffers };
    });

  return {
    servers: {},
    buffers: {},
    order: [],
    active: null,
    channelList: null,
    poppedOut: {},

    openChannelList: (serverId) =>
      set({ channelList: { serverId, entries: [], loading: true } }),
    closeChannelList: () => set({ channelList: null }),

    ensureServer: (serverId, name) =>
      set((s) =>
        s.servers[serverId]
          ? s
          : {
              servers: {
                ...s.servers,
                [serverId]: {
                  id: serverId,
                  name,
                  nick: "",
                  connected: false,
                  registered: false,
                  chanTypes: DEFAULT_CHANTYPES,
                  prefixes: DEFAULT_PREFIXES,
                  prefixModes: "qaohv",
                  caseMapping: "rfc1459",
                  statusMsg: "",
                  chanModes: "beI,k,l,imnpstrS",
                  modesPerLine: 3,
                },
              },
            }
      ),

    ensureBuffer,
    closeBuffer: (key) => {
      const b = get().buffers[key];
      if (b) api.scriptWindowClose(b.serverId, b.kind === "status" ? "Status Window" : b.name).catch(() => {});
      set((s) => {
        const { [key]: _, ...rest } = s.buffers;
        const order = s.order.filter((k) => k !== key);
        const active = s.active === key ? order[order.length - 1] ?? null : s.active;
        return { buffers: rest, order, active };
      });
    },
    renameBuffer: (key, newName) =>
      set((s) => {
        const buf = s.buffers[key];
        if (!buf) return s;
        const newKey = keyFor(buf.serverId, newName);
        if (newKey === key) {
          // Case-only rename: keep the key, just update the display name.
          return { buffers: { ...s.buffers, [key]: { ...buf, name: newName } } };
        }
        const { [key]: _drop, ...others } = s.buffers;
        return {
          buffers: { ...others, [newKey]: { ...buf, key: newKey, name: newName } },
          order: s.order.map((k) => (k === key ? newKey : k)),
          active: s.active === key ? newKey : s.active,
        };
      }),
    closeServer: (serverId) => {
      api.disconnect(serverId).catch(() => {});
      set((s) => {
        const buffers = { ...s.buffers };
        for (const key of Object.keys(buffers)) {
          if (buffers[key].serverId === serverId) delete buffers[key];
        }
        const order = s.order.filter((k) => s.buffers[k]?.serverId !== serverId);
        const { [serverId]: _drop, ...servers } = s.servers;
        const active = s.active && buffers[s.active] ? s.active : order[order.length - 1] ?? null;
        return { buffers, order, servers, active };
      });
    },
    setActive: (key) => {
      // Report the focused window to the script engine (for $active). mIRC names
      // the server console "Status Window"; channels/queries use their own name.
      const b = get().buffers[key];
      if (b) api.scriptSetActive(b.kind === "status" ? "Status Window" : b.name, b.serverId).catch(() => {});
      set((s) =>
        s.buffers[key]
          ? { active: key, buffers: { ...s.buffers, [key]: { ...s.buffers[key], unread: 0, mention: false } } }
          : s
      );
    },
    appendLine,
    handleEvent,
    setPoppedOut: (key, val) =>
      set((s) => ({ poppedOut: { ...s.poppedOut, [key]: val } })),
    addDetachedBuffer: (server, buffer) =>
      set((s) => ({
        servers: { ...s.servers, [server.id]: server },
        buffers: s.buffers[buffer.key] ? s.buffers : { ...s.buffers, [buffer.key]: buffer },
        order: s.order.includes(buffer.key) ? s.order : [...s.order, buffer.key],
        active: buffer.key,
      })),
  };
});

/** Buffer key using the live server's advertised casemapping. Prefer this in
 * UI code which has a server id but is outside the store's event router. */
export const serverBufferKey = (serverId: string, name: string): string =>
  bufferKey(serverId, name, useStore.getState().servers[serverId]?.caseMapping ?? "rfc1459");

function escapeRegex(s: string) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** Returns the action text if `text` is a CTCP ACTION, else null. */
function parseAction(text: string): string | null {
  const m = text.match(/^\x01ACTION (.*?)\x01?$/);
  return m ? m[1] : null;
}
