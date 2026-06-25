import { api } from "./api";
import { bufferKey, Buffer, useStore } from "../state/store";
import { useSettings } from "../state/settings";
import { useUrlGrabber } from "../state/urlGrabber";
import { useChannelCentral } from "../state/channelModes";
import { dccOffers } from "../state/dcc";
import { expandEmoji } from "./emoji";

const ACTION = "\x01ACTION ";

/** Handles a line of user input in the context of the active buffer. */
export async function handleInput(input: string, buffer: Buffer): Promise<void> {
  const text = input.trimEnd();
  if (!text) return;
  const store = useStore.getState();
  const { serverId, name, kind } = buffer;
  const srv = store.servers[serverId];
  const nick = srv?.nick ?? "me";

  // Fire `on INPUT` script handlers for the typed line. If a handler calls
  // /halt, suppress the default send (mIRC behaviour). We await so the decision
  // is known before sending.
  const halted = await api
    .scriptRunInput(serverId, kind === "status" ? "" : name, nick, srv?.name ?? "", text)
    .catch(() => false);
  if (halted) return;

  const echoSelf = (target: string, body: string, lineKind: "msg" | "action") =>
    store.appendLine(serverId, target, kind === "channel" ? "channel" : "query", {
      kind: lineKind,
      from: nick,
      text: body,
      self: true,
    });

  if (!text.startsWith("/")) {
    if (kind === "status") {
      store.appendLine(serverId, name, "status", {
        kind: "error",
        text: "No channel selected. Use /join #channel.",
      });
      return;
    }
    if (kind === "window") {
      // A custom @window has no message target; plain text is delivered only via
      // the `on INPUT` event (already fired above). Nothing else to send.
      return;
    }
    const body = expandEmoji(text);
    if (name.startsWith("=")) {
      // A DCC chat buffer: send over the peer connection, not the server.
      await api.dccSend(name, body).catch(() => {});
      echoSelf(name, body, "msg");
      return;
    }
    await api.sendMessage(serverId, name, body);
    echoSelf(name, body, "msg");
    return;
  }

  // Strip the leading slash; `//cmd` (mIRC's "evaluate then run") is treated
  // like `/cmd` here, so drop a second leading slash too.
  const afterSlash = text.slice(1);
  const line = afterSlash.startsWith("/") ? afterSlash.slice(1) : afterSlash;
  const [cmd, ...rest] = line.split(" ");
  const args = rest.join(" ");
  const command = cmd.toLowerCase();

  switch (command) {
    case "join":
    case "j":
      if (args) await api.join(serverId, args.split(" ")[0]);
      break;
    case "dcc": {
      const sub = rest[0]?.toLowerCase();
      const who = rest[1];
      if (sub === "chat" && who) {
        await api.dccChat(serverId, who).catch(() => {});
      } else if (sub === "close") {
        const id = name.startsWith("=") ? name : who ? `=${who}` : "";
        if (id) await api.dccClose(id).catch(() => {});
      } else if ((sub === "get" || sub === "accept") && who) {
        const offer = dccOffers.take(serverId, who);
        if (offer) await api.dccAccept(serverId, offer.nick, offer.ip, offer.port).catch(() => {});
      }
      break;
    }
    case "part":
    case "leave": {
      const parts = args.split(" ");
      const channel = parts[0]?.startsWith("#") ? parts.shift()! : name;
      await api.part(serverId, channel, parts.join(" ") || undefined);
      break;
    }
    case "msg":
    case "m": {
      const target = rest.shift();
      const body = rest.join(" ");
      if (target && body) {
        await api.sendMessage(serverId, target, body);
        echoSelf(target, body, "msg");
      }
      break;
    }
    case "query":
    case "q":
      if (args) {
        const target = args.split(" ")[0];
        store.setActive(store.ensureBuffer(serverId, target, "query"));
      }
      break;
    case "me":
      if (args && kind !== "status") {
        await api.sendRaw(serverId, `PRIVMSG ${name} :${ACTION}${args}\x01`);
        echoSelf(name, args, "action");
      }
      break;
    case "nick":
      if (args) await api.setNick(serverId, args.split(" ")[0]);
      break;
    case "whois":
    case "wi":
      if (args) await api.whois(serverId, args.split(" ")[0]);
      break;
    case "ignore": {
      const who = rest[0];
      if (who) {
        const st = useSettings.getState();
        if (!st.ignores.includes(who)) st.set("ignores", [...st.ignores, who]);
        store.appendLine(serverId, name, kind, { kind: "system", text: `Ignoring ${who}` });
      }
      break;
    }
    case "unignore": {
      const who = rest[0];
      if (who) {
        const st = useSettings.getState();
        st.set("ignores", st.ignores.filter((i) => i !== who));
        store.appendLine(serverId, name, kind, { kind: "system", text: `No longer ignoring ${who}` });
      }
      break;
    }
    case "topic":
      if (kind === "channel") {
        await api.sendRaw(serverId, args ? `TOPIC ${name} :${args}` : `TOPIC ${name}`);
      }
      break;
    case "names":
      if (kind === "channel") await api.sendRaw(serverId, `NAMES ${name}`);
      break;
    case "list":
      store.openChannelList(serverId);
      await api.sendRaw(serverId, args ? `LIST ${args}` : "LIST");
      break;
    case "listx":
      store.openChannelList(serverId);
      await api.sendRaw(serverId, args ? `LISTX ${args}` : "LISTX");
      break;
    case "op":
    case "deop":
    case "voice":
    case "devoice": {
      const flag = { op: "+o", deop: "-o", voice: "+v", devoice: "-v" }[command]!;
      const who = rest[0];
      if (kind === "channel" && who) await api.sendRaw(serverId, `MODE ${name} ${flag} ${who}`);
      break;
    }
    case "kick":
    case "k": {
      const who = rest.shift();
      const reason = rest.join(" ");
      if (kind === "channel" && who) {
        await api.sendRaw(serverId, reason ? `KICK ${name} ${who} :${reason}` : `KICK ${name} ${who}`);
      }
      break;
    }
    case "ban":
    case "b":
      if (kind === "channel" && rest[0]) {
        const mask = rest[0].includes("!") ? rest[0] : `${rest[0]}!*@*`;
        await api.sendRaw(serverId, `MODE ${name} +b ${mask}`);
      }
      break;
    case "unban":
      if (kind === "channel" && rest[0]) {
        const mask = rest[0].includes("!") ? rest[0] : `${rest[0]}!*@*`;
        await api.sendRaw(serverId, `MODE ${name} -b ${mask}`);
      }
      break;
    case "mode":
      if (args) await api.sendRaw(serverId, `MODE ${args.startsWith("#") ? "" : name + " "}${args}`.trim());
      break;
    case "whisper":
    case "w": {
      const target = rest.shift();
      const body = rest.join(" ");
      if (kind === "channel" && target && body) {
        await api.ircxWhisper(serverId, name, target, body);
        store.appendLine(serverId, name, "channel", {
          kind: "whisper",
          from: nick,
          to: target,
          text: body,
          self: true,
        });
      }
      break;
    }
    case "close":
    case "wc":
      store.closeBuffer(bufferKey(serverId, name));
      break;
    case "quit":
      await api.disconnect(serverId, args || useSettings.getState().quitMessage || undefined);
      break;
    case "raw":
    case "quote":
      if (args) await api.sendRaw(serverId, args);
      break;
    case "socklist": {
      // List open script sockets (optional wildcard filter in args).
      const all = await api.scriptSockets().catch((): string[] => []);
      const filter = args.trim();
      const matched = filter && filter !== "*"
        ? all.filter((s) => s.toLowerCase().includes(filter.toLowerCase().replace(/\*/g, "")))
        : all;
      store.appendLine(serverId, name, kind, {
        kind: "system",
        text: matched.length ? `Sockets: ${matched.join(", ")}` : "No open sockets",
      });
      break;
    }
    case "channel": {
      // Open Channel Central for a channel (defaults to the current one).
      const chan = args.trim().startsWith("#") ? args.trim().split(/\s+/)[0] : kind === "channel" ? name : "";
      if (chan) useChannelCentral.getState().open(serverId, chan);
      break;
    }
    case "url": {
      // Open a URL in the default browser. /url [-switches] <address>.
      const addr = args.trim().split(/\s+/).filter((t) => !t.startsWith("-")).pop();
      if (addr) await api.openUrl(addr);
      break;
    }
    case "exit":
      await api.exitApp();
      break;
    case "dns": {
      const host = args.trim().split(/\s+/)[0];
      if (!host) break;
      store.appendLine(serverId, name, kind, { kind: "system", text: `Resolving ${host}…` });
      api
        .dnsLookup(host)
        .then((ips) =>
          store.appendLine(serverId, name, kind, {
            kind: "system",
            text: ips.length ? `${host} resolves to ${ips.join(", ")}` : `${host}: no addresses found`,
          })
        )
        .catch((e) => store.appendLine(serverId, name, kind, { kind: "error", text: `DNS lookup failed: ${e}` }));
      break;
    }
    case "urls": {
      const grabber = useUrlGrabber.getState();
      if (args.trim().toLowerCase() === "clear") {
        grabber.clear();
        store.appendLine(serverId, name, kind, { kind: "system", text: "URL list cleared." });
        break;
      }
      const urls = grabber.urls;
      if (!urls.length) {
        store.appendLine(serverId, name, kind, { kind: "system", text: "No URLs captured yet." });
      } else {
        store.appendLine(serverId, name, kind, {
          kind: "system",
          text: `Captured URLs (${urls.length}, newest last) — /urls clear to reset:`,
        });
        for (const u of urls.slice(-25)) {
          store.appendLine(serverId, name, kind, { kind: "system", text: `  ${u.url}  — ${u.from} in ${u.buffer}` });
        }
      }
      break;
    }
    case "partall":
      for (const b of Object.values(store.buffers)) {
        if (b.serverId === serverId && b.kind === "channel") await api.part(serverId, b.name, args || undefined);
      }
      break;
    default: {
      // Hand the line to the script engine: it runs built-in script commands
      // (/sockopen, /timer, /hadd, …), then user aliases, then falls back to a
      // raw IRC command for anything it doesn't recognise.
      const network = srv?.name ?? "";
      const target = kind === "status" ? "" : name;
      await api.scriptRunCommand(serverId, target, nick, network, command, args).catch(() => {});
      break;
    }
  }
}
