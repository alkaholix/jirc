import { describe, it, expect, beforeEach, vi } from "vitest";

// The store calls into the Tauri command layer; stub it out for tests.
vi.mock("../lib/api", () => ({
  api: {
    logAppend: vi.fn().mockResolvedValue(undefined),
    logRead: vi.fn().mockResolvedValue(""),
    join: vi.fn().mockResolvedValue(undefined),
    ircxOwnerProtect: vi.fn().mockResolvedValue(undefined),
  },
}));
vi.mock("../lib/notify", () => ({ notify: vi.fn() }));

import { api } from "../lib/api";
import { useStore, bufferKey, STATUS } from "./store";
import { useSettings } from "./settings";

const SID = "s1";

beforeEach(() => {
  useStore.setState({ servers: {}, buffers: {}, order: [], active: null });
  const s = useStore.getState();
  s.ensureServer(SID, "TestNet");
});

describe("connection lifecycle", () => {
  it("records registration and nick", () => {
    useStore.getState().handleEvent({ type: "registered", serverId: SID, nick: "me" });
    const srv = useStore.getState().servers[SID];
    expect(srv.registered).toBe(true);
    expect(srv.nick).toBe("me");
  });
});

describe("channel routing", () => {
  it("routes a % (IRCX) channel message to that channel window", () => {
    const s = useStore.getState();
    s.handleEvent({ type: "isupport", serverId: SID, chanTypes: "%#", prefixes: "~&@%+" });
    s.handleEvent({
      type: "message",
      serverId: SID,
      kind: "privmsg",
      from: ">Bob",
      target: "%#The\\bLobby",
      text: "khkh",
      time: null,
    });
    const lines =
      useStore.getState().buffers[bufferKey(SID, "%#The\\bLobby")]?.lines.map((l) => l.text) ?? [];
    expect(lines).toContain("khkh");
  });

  it("routes % channel messages even before ISUPPORT (default chantypes include %)", () => {
    const s = useStore.getState();
    s.handleEvent({
      type: "message",
      serverId: SID,
      kind: "privmsg",
      from: ">Bob",
      target: "%#room",
      text: "hello",
      time: null,
    });
    const lines =
      useStore.getState().buffers[bufferKey(SID, "%#room")]?.lines.map((l) => l.text) ?? [];
    expect(lines).toContain("hello");
  });

  it("strips the trailing \\x01 from a CTCP ACTION (no leftover box char)", () => {
    const s = useStore.getState();
    s.handleEvent({
      type: "message",
      serverId: SID,
      kind: "privmsg",
      from: ">MightyPenguin81",
      target: "%#the\\blobby",
      text: "\x01ACTION eewwe\x01",
      time: null,
    });
    const line = useStore.getState().buffers[bufferKey(SID, "%#the\\blobby")]?.lines.at(-1);
    expect(line?.kind).toBe("action");
    expect(line?.text).toBe("eewwe");
  });

  it("drops messages from an ignored sender, but mode/events still show", () => {
    useSettings.getState().set("ignores", ["Spammer"]);
    const s = useStore.getState();
    s.handleEvent({ type: "message", serverId: SID, kind: "privmsg", from: "Spammer", target: "#c", text: "spam", time: null });
    s.handleEvent({ type: "mode", serverId: SID, target: "#c", modes: "+o Spammer", by: "Spammer" });
    const lines = useStore.getState().buffers[bufferKey(SID, "#c")]?.lines.map((l) => l.text) ?? [];
    expect(lines).not.toContain("spam");
    expect(lines.some((t) => t.includes("sets mode"))).toBe(true);
    useSettings.getState().set("ignores", []);
  });
});

describe("mode display", () => {
  it("attributes a channel mode to whoever set it", () => {
    useStore.getState().handleEvent({
      type: "mode",
      serverId: SID,
      target: "#chan",
      modes: "+v Bob",
      by: "Snue",
    });
    const lines =
      useStore.getState().buffers[bufferKey(SID, "#chan")]?.lines.map((l) => l.text) ?? [];
    expect(lines).toContain("Snue sets mode: +v Bob");
  });
});

describe("numeric gating", () => {
  const statusLines = () =>
    useStore.getState().buffers[bufferKey(SID, STATUS)]?.lines.map((l) => l.text) ?? [];

  it("hides informational numerics unless trace, always shows errors", () => {
    useStore
      .getState()
      .handleEvent({ type: "numeric", serverId: SID, code: 251, args: ["me", "5 users"] });
    expect(statusLines().some((t) => t.includes("251"))).toBe(false);

    useStore.getState().handleEvent({
      type: "numeric",
      serverId: SID,
      code: 433,
      args: ["me", "nick", "Nickname is already in use"],
    });
    expect(statusLines().some((t) => t.includes("433"))).toBe(true);
  });
});

describe("channel membership", () => {
  beforeEach(() => {
    useStore.getState().handleEvent({ type: "registered", serverId: SID, nick: "me" });
  });

  it("creates and activates a channel on self-join", () => {
    useStore.getState().handleEvent({ type: "join", serverId: SID, channel: "#x", nick: "me" });
    const key = bufferKey(SID, "#x");
    const st = useStore.getState();
    expect(st.buffers[key]).toBeTruthy();
    expect(st.active).toBe(key);
    expect(st.buffers[key].members.map((m) => m.nick)).toContain("me");
  });

  it("sorts by server-advertised prefixes (IRCX .@+)", () => {
    useStore
      .getState()
      .handleEvent({ type: "isupport", serverId: SID, chanTypes: "#&", prefixes: ".@+" });
    useStore.getState().handleEvent({ type: "join", serverId: SID, channel: "#x", nick: "me" });
    useStore.getState().handleEvent({
      type: "names",
      serverId: SID,
      channel: "#x",
      members: [
        { nick: "v", prefix: "+" },
        { nick: "owner", prefix: "." },
        { nick: "op", prefix: "@" },
        { nick: "plain", prefix: "" },
      ],
    });
    const members = useStore.getState().buffers[bufferKey(SID, "#x")].members;
    expect(members.map((m) => m.nick)).toEqual(["owner", "op", "v", "plain"]);
  });

  it("sorts NAMES by prefix rank", () => {
    useStore.getState().handleEvent({ type: "join", serverId: SID, channel: "#x", nick: "me" });
    useStore.getState().handleEvent({
      type: "names",
      serverId: SID,
      channel: "#x",
      members: [
        { nick: "bob", prefix: "" },
        { nick: "carol", prefix: "@" },
        { nick: "dave", prefix: "+" },
      ],
    });
    const members = useStore.getState().buffers[bufferKey(SID, "#x")].members;
    expect(members.map((m) => m.nick)).toEqual(["carol", "dave", "bob"]);
  });

  it("removes a member on part", () => {
    useStore.getState().handleEvent({ type: "join", serverId: SID, channel: "#x", nick: "me" });
    useStore.getState().handleEvent({ type: "join", serverId: SID, channel: "#x", nick: "bob" });
    useStore.getState().handleEvent({ type: "part", serverId: SID, channel: "#x", nick: "bob", reason: null });
    const members = useStore.getState().buffers[bufferKey(SID, "#x")].members;
    expect(members.map((m) => m.nick)).not.toContain("bob");
  });
});

describe("messages", () => {
  beforeEach(() => {
    useStore.getState().handleEvent({ type: "registered", serverId: SID, nick: "me" });
    // Keep the status buffer active so #x stays unread.
    const s = useStore.getState();
    s.ensureBuffer(SID, STATUS, "status");
    s.setActive(bufferKey(SID, STATUS));
  });

  it("appends channel messages and counts unread", () => {
    useStore.getState().handleEvent({
      type: "message",
      serverId: SID,
      kind: "privmsg",
      from: "bob",
      target: "#x",
      text: "hello there",
      time: null,
    });
    const buf = useStore.getState().buffers[bufferKey(SID, "#x")];
    expect(buf.lines.at(-1)?.text).toBe("hello there");
    expect(buf.unread).toBe(1);
    expect(buf.mention).toBe(false);
  });

  it("flags mentions of our nick", () => {
    useStore.getState().handleEvent({
      type: "message",
      serverId: SID,
      kind: "privmsg",
      from: "bob",
      target: "#x",
      text: "me: ping",
      time: null,
    });
    expect(useStore.getState().buffers[bufferKey(SID, "#x")].mention).toBe(true);
  });

  it("drops messages from ignored nicks", async () => {
    const { useSettings } = await import("../state/settings");
    useSettings.getState().set("ignores", ["spammer"]);
    useStore.getState().handleEvent({
      type: "message",
      serverId: SID,
      kind: "privmsg",
      from: "spammer",
      target: "#x",
      text: "buy now",
      time: null,
    });
    expect(useStore.getState().buffers[bufferKey(SID, "#x")]).toBeUndefined();
    useSettings.getState().set("ignores", []);
  });

  it("routes direct messages to a query buffer", () => {
    useStore.getState().handleEvent({
      type: "message",
      serverId: SID,
      kind: "privmsg",
      from: "bob",
      target: "me",
      text: "hi privately",
      time: null,
    });
    const buf = useStore.getState().buffers[bufferKey(SID, "bob")];
    expect(buf?.kind).toBe("query");
    expect(buf.lines.at(-1)?.text).toBe("hi privately");
  });
});

describe("custom @windows", () => {
  const winLines = (name: string) =>
    useStore.getState().buffers[bufferKey(SID, name)]?.lines.map((l) => l.text) ?? [];

  it("opens a window buffer, activates it, and records its kind", () => {
    useStore
      .getState()
      .handleEvent({ type: "windowOpen", serverId: SID, name: "@list", kind: "listbox", title: "My List" });
    const key = bufferKey(SID, "@list");
    const buf = useStore.getState().buffers[key];
    expect(buf?.kind).toBe("window");
    expect(buf.windowKind).toBe("listbox");
    expect(useStore.getState().active).toBe(key);
  });

  it("mirrors aline/iline/rline/dline/clear with 1-based positions", () => {
    const s = useStore.getState();
    s.handleEvent({ type: "windowOpen", serverId: SID, name: "@w", kind: "listbox", title: "@w" });
    const add = (text: string) =>
      s.handleEvent({ type: "windowLine", serverId: SID, name: "@w", op: "add", n: 0, text });
    add("one");
    add("two");
    add("three");
    expect(winLines("@w")).toEqual(["one", "two", "three"]);

    s.handleEvent({ type: "windowLine", serverId: SID, name: "@w", op: "replace", n: 2, text: "TWO" });
    expect(winLines("@w")).toEqual(["one", "TWO", "three"]);

    s.handleEvent({ type: "windowLine", serverId: SID, name: "@w", op: "insert", n: 1, text: "zero" });
    expect(winLines("@w")).toEqual(["zero", "one", "TWO", "three"]);

    s.handleEvent({ type: "windowLine", serverId: SID, name: "@w", op: "delete", n: 1, text: "" });
    expect(winLines("@w")).toEqual(["one", "TWO", "three"]);

    s.handleEvent({ type: "windowLine", serverId: SID, name: "@w", op: "clear", n: 0, text: "" });
    expect(winLines("@w")).toEqual([]);
  });

  it("recreates the window buffer if a line op arrives before it was opened", () => {
    useStore
      .getState()
      .handleEvent({ type: "windowLine", serverId: SID, name: "@auto", op: "add", n: 0, text: "hi" });
    const buf = useStore.getState().buffers[bufferKey(SID, "@auto")];
    expect(buf?.kind).toBe("window");
    expect(winLines("@auto")).toEqual(["hi"]);
  });

  it("does not steal focus when /window re-opens an existing window", () => {
    const s = useStore.getState();
    s.handleEvent({ type: "windowOpen", serverId: SID, name: "@dash", kind: "listbox", title: "@dash" });
    s.ensureBuffer(SID, STATUS, "status");
    s.setActive(bufferKey(SID, STATUS));
    s.handleEvent({ type: "windowOpen", serverId: SID, name: "@dash", kind: "listbox", title: "@dash" });
    expect(useStore.getState().active).toBe(bufferKey(SID, STATUS));
  });

  it("closes a window buffer", () => {
    const s = useStore.getState();
    s.handleEvent({ type: "windowOpen", serverId: SID, name: "@gone", kind: "listbox", title: "@gone" });
    expect(useStore.getState().buffers[bufferKey(SID, "@gone")]).toBeTruthy();
    s.handleEvent({ type: "windowClose", serverId: SID, name: "@gone" });
    expect(useStore.getState().buffers[bufferKey(SID, "@gone")]).toBeUndefined();
  });
});

describe("renameBuffer (/queryrn)", () => {
  it("moves a query buffer to a new key, preserving lines + active", () => {
    const s = useStore.getState();
    s.appendLine(SID, "bob", "query", { kind: "msg", from: "bob", text: "hi there" });
    const oldKey = bufferKey(SID, "bob");
    s.setActive(oldKey);
    s.renameBuffer(oldKey, "robert");

    const st = useStore.getState();
    const newKey = bufferKey(SID, "robert");
    expect(st.buffers[oldKey]).toBeUndefined();
    expect(st.buffers[newKey]?.name).toBe("robert");
    expect(st.buffers[newKey]?.lines.map((l) => l.text)).toContain("hi there");
    expect(st.active).toBe(newKey);
    expect(st.order).toContain(newKey);
    expect(st.order).not.toContain(oldKey);
  });
});

describe("IRCX owner protection", () => {
  it("runs takeover protection when someone else strips our +q", () => {
    useStore
      .getState()
      .handleEvent({ type: "ownerRevoked", serverId: SID, channel: "%#room", by: "taker" });
    expect(api.ircxOwnerProtect).toHaveBeenCalledWith(SID, "TestNet", "%#room", "taker");
  });
});
