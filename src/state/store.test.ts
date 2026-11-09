import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

// The store calls into the Tauri command layer; stub it out for tests.
vi.mock("../lib/api", () => ({
  api: {
    logAppend: vi.fn().mockResolvedValue(undefined),
    logRead: vi.fn().mockResolvedValue(""),
    join: vi.fn().mockResolvedValue(undefined),
    ircxOwnerProtect: vi.fn().mockResolvedValue(undefined),
    disconnect: vi.fn().mockResolvedValue(undefined),
    scriptSetActive: vi.fn().mockResolvedValue(undefined),
    scriptWindowOpen: vi.fn().mockResolvedValue(undefined),
    scriptWindowClose: vi.fn().mockResolvedValue(undefined),
    scriptWindowRename: vi.fn().mockResolvedValue(undefined),
  },
}));
vi.mock("../lib/notify", () => ({ notify: vi.fn() }));

import { api } from "../lib/api";
import { useStore, bufferKey, ircCasefold, serverBufferKey, STATUS } from "./store";
import { useSettings } from "./settings";

const SID = "s1";

beforeEach(() => {
  vi.clearAllMocks();
  useSettings.getState().set("autoJoinInvites", false);
  useStore.setState({ servers: {}, buffers: {}, order: [], active: null });
  const s = useStore.getState();
  s.ensureServer(SID, "TestNet");
});

describe("invite compatibility", () => {
  it("auto-joins invited channels only when /ajinvite enabled it", () => {
    useSettings.getState().set("autoJoinInvites", true);
    useStore.getState().handleEvent({ type: "invite", serverId: SID, from: "Alice", channel: "#welcome" });
    expect(api.join).toHaveBeenCalledWith(SID, "#welcome");
  });
});

describe("connection lifecycle", () => {
  it("unregisters a closed server's windows and reports the actual fallback focus", () => {
    const s = useStore.getState();
    s.ensureBuffer(SID, STATUS, "status");
    const query = s.ensureBuffer(SID, "Alice", "query");
    s.ensureServer("other", "Other");
    const fallback = s.ensureBuffer("other", "@fallback", "window");
    s.setActive(query);
    vi.clearAllMocks();
    s.closeServer(SID);
    expect(useStore.getState().active).toBe(fallback);
    expect(api.scriptWindowClose).toHaveBeenCalledTimes(2);
    expect(api.scriptWindowClose).toHaveBeenCalledWith(SID, "Status Window");
    expect(api.scriptWindowClose).toHaveBeenCalledWith(SID, "Alice");
    expect(api.scriptSetActive).toHaveBeenCalledTimes(1);
    expect(api.scriptSetActive).toHaveBeenCalledWith("@fallback", "other");
  });

  it("does not refocus when closing an inactive server", () => {
    const s = useStore.getState();
    s.ensureBuffer(SID, "Alice", "query");
    s.ensureServer("other", "Other");
    const active = s.ensureBuffer("other", "Bob", "query");
    s.setActive(active);
    vi.clearAllMocks();
    s.closeServer(SID);
    expect(useStore.getState().active).toBe(active);
    expect(api.scriptSetActive).not.toHaveBeenCalled();
    expect(api.scriptWindowClose).toHaveBeenCalledWith(SID, "Alice");
  });

  it("unregisters the last server without reporting a nonexistent fallback", () => {
    const s = useStore.getState();
    const active = s.ensureBuffer(SID, STATUS, "status");
    s.setActive(active);
    vi.clearAllMocks();
    s.closeServer(SID);
    expect(useStore.getState().active).toBeNull();
    expect(api.scriptWindowClose).toHaveBeenCalledWith(SID, "Status Window");
    expect(api.scriptSetActive).not.toHaveBeenCalled();
  });

  it("reports the visible fallback window when the active custom window closes", () => {
    const store = useStore.getState();
    store.ensureBuffer(SID, STATUS, "status");
    store.handleEvent({ type: "windowOpen", serverId: SID, name: "@one", kind: "editbox", title: "@one" });
    store.handleEvent({ type: "windowOpen", serverId: SID, name: "@two", kind: "editbox", title: "@two" });
    store.setActive(serverBufferKey(SID, "@one"));
    vi.mocked(api.scriptSetActive).mockClear();
    store.handleEvent({ type: "windowClose", serverId: SID, name: "@one" });
    expect(useStore.getState().active).toBe(serverBufferKey(SID, "@two"));
    expect(api.scriptSetActive).toHaveBeenCalledTimes(1);
    expect(api.scriptSetActive).toHaveBeenCalledWith("@two", SID);
    store.closeBuffer(serverBufferKey(SID, "@two"));
    expect(api.scriptSetActive).toHaveBeenLastCalledWith("Status Window", SID);
  });

  it("keeps the current focus notification unchanged when an inactive window closes", () => {
    const store = useStore.getState();
    store.handleEvent({ type: "windowOpen", serverId: SID, name: "@one", kind: "listbox", title: "@one" });
    store.handleEvent({ type: "windowOpen", serverId: SID, name: "@two", kind: "listbox", title: "@two" });
    vi.mocked(api.scriptSetActive).mockClear();
    store.closeBuffer(serverBufferKey(SID, "@one"));
    expect(useStore.getState().active).toBe(serverBufferKey(SID, "@two"));
    expect(api.scriptSetActive).not.toHaveBeenCalled();
  });

  it("records registration and nick", () => {
    useStore.getState().handleEvent({ type: "registered", serverId: SID, nick: "me" });
    const srv = useStore.getState().servers[SID];
    expect(srv.registered).toBe(true);
    expect(srv.nick).toBe("me");
  });

  it("uses NETWORK as the live label without requiring a profile rename", () => {
    useStore.getState().handleEvent({
      type: "isupport",
      serverId: SID,
      network: "IRC7",
      chanTypes: "%#",
      prefixes: ".@+",
      prefixModes: "qov",
      caseMapping: "rfc1459",
      statusMsg: "",
      chanModes: "beI,k,l,imnst",
      modesPerLine: 3,
        monitor: false,
        monitorLimit: 0,
    });
    expect(useStore.getState().servers[SID].name).toBe("IRC7");
  });
});

describe("IRC casemapping", () => {
  it("implements RFC1459, strict RFC1459, and ASCII folding", () => {
    expect(ircCasefold("Nick[\\^", "rfc1459")).toBe("nick{|~");
    expect(ircCasefold("Nick[\\^", "strict-rfc1459")).toBe("nick{|^");
    expect(ircCasefold("Nick[\\^", "ascii")).toBe("nick[\\^");
  });

  it("uses ASCII identity after the server advertises it", () => {
    useStore.getState().handleEvent({
      type: "isupport",
      network: null,
      serverId: SID,
      chanTypes: "#&",
      prefixes: "~&@%+",
      prefixModes: "qaohv",
      caseMapping: "ascii",
      statusMsg: "@+",
      chanModes: "beI,k,l,imnst",
      modesPerLine: 4,
      monitor: false,
      monitorLimit: 0,
    });
    expect(serverBufferKey(SID, "Nick[")).not.toBe(serverBufferKey(SID, "nick{"));
  });
});

describe("channel routing", () => {
  it("routes STATUSMSG targets into the bare channel buffer", () => {
    const s = useStore.getState();
    s.handleEvent({
      type: "isupport",
      network: null,
      serverId: SID,
      chanTypes: "#&",
      prefixes: "~&@%+",
      prefixModes: "qaohv",
      caseMapping: "rfc1459",
      statusMsg: "@+",
      chanModes: "beI,k,l,imnst",
      modesPerLine: 4,
      monitor: false,
      monitorLimit: 0,
    });
    s.handleEvent({
      type: "message",
      serverId: SID,
      kind: "notice",
      from: "op",
      target: "@#Room",
      text: "operators only",
      time: null,
    });

    const state = useStore.getState();
    expect(state.buffers[bufferKey(SID, "#room")]?.lines.at(-1)?.text).toBe("operators only");
    expect(state.buffers[bufferKey(SID, "@#room")]).toBeUndefined();
  });

  it("uses the advertised casemapping for channel buffer identity", () => {
    const s = useStore.getState();
    s.handleEvent({ type: "join", serverId: SID, channel: "#Room[", nick: "someone" });
    s.handleEvent({
      type: "message",
      serverId: SID,
      kind: "privmsg",
      from: "someone",
      target: "#room{",
      text: "same RFC1459 channel",
      time: null,
    });

    const channels = Object.values(useStore.getState().buffers).filter((b) => b.kind === "channel");
    expect(channels).toHaveLength(1);
    expect(channels[0].lines.at(-1)?.text).toBe("same RFC1459 channel");
  });

  it("routes a % (IRCX) channel message to that channel window", () => {
    const s = useStore.getState();
    s.handleEvent({
      type: "isupport",
      network: null,
      serverId: SID,
      chanTypes: "%#",
      prefixes: "~&@%+",
      prefixModes: "qaohv",
      caseMapping: "rfc1459",
      statusMsg: "@+",
      chanModes: "beI,k,l,imnst",
      modesPerLine: 4,
      monitor: false,
      monitorLimit: 0,
    });
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
      .handleEvent({
        type: "isupport",
        network: null,
        serverId: SID,
        chanTypes: "#&",
        prefixes: ".@+",
        prefixModes: "qov",
        caseMapping: "rfc1459",
        statusMsg: "@+",
        chanModes: "beI,k,l,imnst",
        modesPerLine: 4,
        monitor: false,
        monitorLimit: 0,
      });
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

  it("matches RFC1459-equivalent nick spellings in membership events", () => {
    const s = useStore.getState();
    s.handleEvent({ type: "join", serverId: SID, channel: "#x", nick: "User[" });
    s.handleEvent({ type: "part", serverId: SID, channel: "#x", nick: "user{", reason: null });
    expect(useStore.getState().buffers[bufferKey(SID, "#x")].members).toEqual([]);
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

  it("updates a custom window title without changing its stable buffer name", () => {
    const state = useStore.getState();
    state.handleEvent({
      type: "windowOpen",
      serverId: SID,
      name: "@notes",
      kind: "listbox",
      title: "@notes",
    });
    state.handleEvent({
      type: "windowTitle",
      serverId: SID,
      name: "@notes",
      title: "Saved notes",
    });
    const buffer = useStore.getState().buffers[bufferKey(SID, "@notes")];
    expect(buffer.name).toBe("@notes");
    expect(buffer.windowTitle).toBe("Saved notes");
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

    s.handleEvent({ type: "windowLine", serverId: SID, name: "@w", op: "select", n: 2, text: "" });
    s.handleEvent({ type: "windowLine", serverId: SID, name: "@w", op: "selectAdd", n: 3, text: "" });
    expect(useStore.getState().buffers[bufferKey(SID, "@w")]?.windowSelected).toEqual([2, 3]);

    s.handleEvent({ type: "windowLine", serverId: SID, name: "@w", op: "replace", n: 2, text: "TWO" });
    expect(winLines("@w")).toEqual(["one", "TWO", "three"]);

    s.handleEvent({ type: "windowLine", serverId: SID, name: "@w", op: "insert", n: 1, text: "zero" });
    expect(winLines("@w")).toEqual(["zero", "one", "TWO", "three"]);
    expect(useStore.getState().buffers[bufferKey(SID, "@w")]?.windowSelected).toEqual([3, 4]);

    s.handleEvent({ type: "windowLine", serverId: SID, name: "@w", op: "delete", n: 1, text: "" });
    expect(winLines("@w")).toEqual(["one", "TWO", "three"]);
    expect(useStore.getState().buffers[bufferKey(SID, "@w")]?.windowSelected).toEqual([2, 3]);

    s.handleEvent({ type: "windowLine", serverId: SID, name: "@w", op: "deselect", n: 2, text: "" });
    expect(useStore.getState().buffers[bufferKey(SID, "@w")]?.windowSelected).toEqual([3]);

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

  it("retains picture drawing operations and replaces the canvas size", () => {
    const s = useStore.getState();
    s.handleEvent({ type: "windowOpen", serverId: SID, name: "@pic", kind: "picture", title: "@pic" });
    s.handleEvent({ type: "windowDraw", serverId: SID, name: "@pic", op: "drawsize", args: ["", "640", "480"] });
    s.handleEvent({ type: "windowDraw", serverId: SID, name: "@pic", op: "drawline", args: ["", "4", "2", "0", "0", "20", "20"] });
    s.handleEvent({ type: "windowDraw", serverId: SID, name: "@pic", op: "drawsize", args: ["", "800", "600"] });
    expect(useStore.getState().buffers[bufferKey(SID, "@pic")]?.windowDrawing).toEqual([
      { op: "drawline", args: ["", "4", "2", "0", "0", "20", "20"] },
      { op: "drawsize", args: ["", "800", "600"] },
    ]);
  });

  it("does not steal focus when /window re-opens an existing window", () => {
    const s = useStore.getState();
    s.handleEvent({ type: "windowOpen", serverId: SID, name: "@dash", kind: "listbox", title: "@dash" });
    s.ensureBuffer(SID, STATUS, "status");
    s.setActive(bufferKey(SID, STATUS));
    s.handleEvent({ type: "windowOpen", serverId: SID, name: "@dash", kind: "listbox", title: "@dash" });
    expect(useStore.getState().active).toBe(bufferKey(SID, STATUS));
  });

  it("activates an existing picture on request and preserves its drawing and editbox", () => {
    const s = useStore.getState();
    s.handleEvent({ type: "windowOpen", serverId: SID, name: "@picture", kind: "picture", title: "Picture", editbox: true });
    s.handleEvent({ type: "windowDraw", serverId: SID, name: "@picture", op: "drawline", args: ["", "4", "1", "0", "0", "20", "20"] });
    s.ensureBuffer(SID, STATUS, "status");
    s.setActive(bufferKey(SID, STATUS));
    s.handleEvent({ type: "windowOpen", serverId: SID, name: "@picture", kind: "listbox", title: "@picture", activate: true });
    const buffer = useStore.getState().buffers[bufferKey(SID, "@picture")];
    expect(useStore.getState().active).toBe(bufferKey(SID, "@picture"));
    expect(buffer.windowKind).toBe("picture");
    expect(buffer.windowEditbox).toBe(true);
    expect(buffer.windowTitle).toBe("Picture");
    expect(buffer.windowDrawing).toHaveLength(1);
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
  it("reports a renamed active query atomically instead of reopening it", () => {
    const s = useStore.getState();
    const oldKey = s.ensureBuffer(SID, "Alice", "query");
    s.setActive(oldKey);
    vi.clearAllMocks();
    s.renameBuffer(oldKey, "Bob");
    expect(useStore.getState().active).toBe(serverBufferKey(SID, "Bob"));
    expect(api.scriptWindowRename).toHaveBeenCalledWith(SID, "Alice", "Bob");
    expect(api.scriptWindowOpen).not.toHaveBeenCalled();
    expect(api.scriptWindowClose).not.toHaveBeenCalled();
    expect(api.scriptSetActive).not.toHaveBeenCalled();
  });

  it("reports case-only renames and retains a single destination on collision", () => {
    const s = useStore.getState();
    const alice = s.ensureBuffer(SID, "Alice", "query");
    s.renameBuffer(alice, "ALICE");
    expect(useStore.getState().buffers[alice]?.name).toBe("ALICE");
    expect(api.scriptWindowRename).toHaveBeenLastCalledWith(SID, "Alice", "ALICE");
    const bob = s.ensureBuffer(SID, "Bob[", "query");
    s.appendLine(SID, "ALICE", "query", { kind: "msg", text: "source" });
    s.setActive(bob);
    s.renameBuffer(alice, "Bob{");
    const key = serverBufferKey(SID, "Bob{");
    expect(useStore.getState().active).toBe(key);
    expect(useStore.getState().buffers[key]?.lines[0]?.text).toBe("source");
    expect(useStore.getState().order.filter((entry) => entry === key)).toHaveLength(1);
  });

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

describe("rejoin on kick / disconnect", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.clearAllTimers();
    vi.useRealTimers();
  });
  const connectReady = (serverId = SID, autojoinSkip = false, autojoinDelaySecs = 0, autojoinChannels: string[] = []) =>
    useStore.getState().handleEvent({ type: "connectReady", serverId, autojoinSkip, autojoinDelaySecs, autojoinChannels });
  const kickSelf = (channel: string) =>
    useStore.getState().handleEvent({
      type: "kick",
      serverId: SID,
      channel,
      nick: "me",
      by: "op",
      reason: "bye",
      isSelf: true,
    });

  it("rejoins channels after a disconnect — even if the windows were closed", () => {
    useSettings.getState().set("rejoinOnReconnect", true);
    useSettings.getState().set("keepOpenOnKickQuit", false); // windows close on disconnect
    const s = useStore.getState();
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    s.handleEvent({ type: "join", serverId: SID, channel: "#chan", nick: "me" });
    vi.mocked(api.join).mockClear();

    s.handleEvent({ type: "disconnected", serverId: SID, reason: "ping timeout" });
    // The channel window is gone, but the channel was remembered…
    expect(useStore.getState().buffers[bufferKey(SID, "#chan")]).toBeUndefined();
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    expect(api.join).not.toHaveBeenCalled();
    connectReady();
    expect(api.join).toHaveBeenCalledWith(SID, "#chan");
  });

  it("does not rejoin after a disconnect when the setting is off", () => {
    useSettings.getState().set("rejoinOnReconnect", false);
    useSettings.getState().set("keepOpenOnKickQuit", true);
    const s = useStore.getState();
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    s.handleEvent({ type: "join", serverId: SID, channel: "#chan", nick: "me" });
    vi.mocked(api.join).mockClear();
    s.handleEvent({ type: "disconnected", serverId: SID, reason: "x" });
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    connectReady();
    expect(api.join).not.toHaveBeenCalled();
  });

  it("does not rejoin open windows on the first connection", () => {
    useSettings.getState().set("rejoinOnReconnect", true);
    const s = useStore.getState();
    s.ensureBuffer(SID, "#open", "channel");
    s.handleEvent({ type: "connected", serverId: SID });
    connectReady(); // Readiness before registration is ignored.
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    connectReady();
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    connectReady();
    expect(api.join).not.toHaveBeenCalled();
  });

  it("rejoins the remembered/open union once after CONNECT readiness", () => {
    useSettings.getState().set("rejoinOnReconnect", true);
    useSettings.getState().set("keepOpenOnKickQuit", true);
    const s = useStore.getState();
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    connectReady();
    s.handleEvent({ type: "join", serverId: SID, channel: "#remembered", nick: "me" });
    s.handleEvent({ type: "disconnected", serverId: SID, reason: "x" });
    s.ensureBuffer(SID, "#open", "channel");
    s.handleEvent({ type: "connected", serverId: SID });
    connectReady();
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    expect(api.join).not.toHaveBeenCalled();
    connectReady();
    expect(api.join).toHaveBeenCalledTimes(2);
    expect(api.join).toHaveBeenCalledWith(SID, "#remembered");
    expect(api.join).toHaveBeenCalledWith(SID, "#open");
    connectReady();
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    connectReady();
    expect(api.join).toHaveBeenCalledTimes(2);
  });

  it("leaves profile autojoin to the backend and rejoins only additional channels", () => {
    useSettings.getState().set("rejoinOnReconnect", true);
    useSettings.getState().set("keepOpenOnKickQuit", true);
    const s = useStore.getState();
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    s.handleEvent({ type: "join", serverId: SID, channel: "#profile", nick: "me" });
    s.handleEvent({ type: "join", serverId: SID, channel: "#remembered", nick: "me" });
    s.handleEvent({ type: "disconnected", serverId: SID, reason: "x" });
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    connectReady(SID, false, 0, ["#profile"]);
    expect(api.join).toHaveBeenCalledTimes(1);
    expect(api.join).toHaveBeenCalledWith(SID, "#remembered");
    connectReady(SID, false, 0, ["#profile"]);
    expect(api.join).toHaveBeenCalledTimes(1);
  });

  it("matches profile channels using IRC casemapping without treating keys as channels", () => {
    useSettings.getState().set("rejoinOnReconnect", true);
    const s = useStore.getState();
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    s.handleEvent({ type: "join", serverId: SID, channel: "#Room[", nick: "me" });
    s.handleEvent({ type: "join", serverId: SID, channel: "#key", nick: "me" });
    s.handleEvent({ type: "disconnected", serverId: SID, reason: "x" });
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    connectReady(SID, false, 0, ["  #ROOM{ #key  "]);
    expect(api.join).toHaveBeenCalledTimes(1);
    expect(api.join).toHaveBeenCalledWith(SID, "#key");
  });

  it("recognizes comma-separated channel portions in a keyed profile join", () => {
    useSettings.getState().set("rejoinOnReconnect", true);
    const s = useStore.getState();
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    for (const channel of ["#first", "#second", "#extra"]) {
      s.handleEvent({ type: "join", serverId: SID, channel, nick: "me" });
    }
    s.handleEvent({ type: "disconnected", serverId: SID, reason: "x" });
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    connectReady(SID, false, 0, ["#FIRST,#second key1,key2"]);
    expect(api.join).toHaveBeenCalledTimes(1);
    expect(api.join).toHaveBeenCalledWith(SID, "#extra");
  });

  it("uses the connection's advertised ASCII mapping for profile overlap", () => {
    useSettings.getState().set("rejoinOnReconnect", true);
    const s = useStore.getState();
    s.handleEvent({
      type: "isupport", serverId: SID, network: null, chanTypes: "#&", prefixes: "~&@%+",
      prefixModes: "qaohv", caseMapping: "ascii", statusMsg: "", chanModes: "beI,k,l,imnst",
      modesPerLine: 3, monitor: false, monitorLimit: 0,
    });
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    s.handleEvent({ type: "join", serverId: SID, channel: "#Room[", nick: "me" });
    s.handleEvent({ type: "join", serverId: SID, channel: "#Exact", nick: "me" });
    s.handleEvent({ type: "disconnected", serverId: SID, reason: "x" });
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    connectReady(SID, false, 0, ["#room{", "#EXACT"]);
    expect(api.join).toHaveBeenCalledTimes(1);
    expect(api.join).toHaveBeenCalledWith(SID, "#Room[");
  });

  it("deduplicates differently spelled remembered and open channel names", () => {
    useSettings.getState().set("rejoinOnReconnect", true);
    useSettings.getState().set("keepOpenOnKickQuit", false);
    const s = useStore.getState();
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    s.handleEvent({ type: "join", serverId: SID, channel: "#Room[", nick: "me" });
    s.handleEvent({ type: "disconnected", serverId: SID, reason: "x" });
    s.ensureBuffer(SID, "#room{", "channel");
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    connectReady();
    expect(api.join).toHaveBeenCalledTimes(1);
    expect(api.join).toHaveBeenCalledWith(SID, "#Room[");
  });

  it("retains backend-managed remembered channels if a delayed attempt is interrupted", () => {
    useSettings.getState().set("rejoinOnReconnect", true);
    useSettings.getState().set("keepOpenOnKickQuit", false);
    const s = useStore.getState();
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    for (const channel of ["#profile", "#remembered"]) {
      s.handleEvent({ type: "join", serverId: SID, channel, nick: "me" });
    }
    s.handleEvent({ type: "disconnected", serverId: SID, reason: "x" });
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    connectReady(SID, false, 2, ["#profile"]);
    s.handleEvent({ type: "disconnected", serverId: SID, reason: "before delayed joins" });
    vi.advanceTimersByTime(2000);
    expect(api.join).not.toHaveBeenCalled();
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    // The next connection no longer has a profile autojoin for this channel.
    connectReady();
    expect(api.join).toHaveBeenCalledTimes(2);
    expect(api.join).toHaveBeenCalledWith(SID, "#profile");
    expect(api.join).toHaveBeenCalledWith(SID, "#remembered");
  });

  it("honours CONNECT's autojoin skip for reconnect joins", () => {
    useSettings.getState().set("rejoinOnReconnect", true);
    const s = useStore.getState();
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    s.handleEvent({ type: "join", serverId: SID, channel: "#chan", nick: "me" });
    s.handleEvent({ type: "disconnected", serverId: SID, reason: "x" });
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    connectReady(SID, true, 0, ["#profile"]);
    connectReady();
    expect(api.join).not.toHaveBeenCalled();
  });

  it("honours CONNECT's autojoin delay without scheduling duplicate joins", () => {
    useSettings.getState().set("rejoinOnReconnect", true);
    const s = useStore.getState();
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    s.handleEvent({ type: "join", serverId: SID, channel: "#chan", nick: "me" });
    s.handleEvent({ type: "disconnected", serverId: SID, reason: "x" });
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    connectReady(SID, false, 2, ["#profile"]);
    connectReady(SID, false, 2, ["#profile"]);
    vi.advanceTimersByTime(1999);
    expect(api.join).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(api.join).toHaveBeenCalledTimes(1);
    expect(api.join).toHaveBeenCalledWith(SID, "#chan");
  });

  it("cancels old delayed joins across a disconnect and replacement session", () => {
    useSettings.getState().set("rejoinOnReconnect", true);
    useSettings.getState().set("keepOpenOnKickQuit", true);
    const s = useStore.getState();
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    s.handleEvent({ type: "join", serverId: SID, channel: "#chan", nick: "me" });
    s.handleEvent({ type: "disconnected", serverId: SID, reason: "x" });
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    connectReady(SID, false, 2);
    s.handleEvent({ type: "disconnected", serverId: SID, reason: "again" });
    connectReady();
    s.handleEvent({ type: "connected", serverId: SID });
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    vi.advanceTimersByTime(2000);
    expect(api.join).not.toHaveBeenCalled();
    connectReady();
    expect(api.join).toHaveBeenCalledTimes(1);
    expect(api.join).toHaveBeenCalledWith(SID, "#chan");
  });

  it("does not let another server's readiness trigger pending joins", () => {
    useSettings.getState().set("rejoinOnReconnect", true);
    const s = useStore.getState();
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    s.handleEvent({ type: "join", serverId: SID, channel: "#chan", nick: "me" });
    s.handleEvent({ type: "disconnected", serverId: SID, reason: "x" });
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    s.ensureServer("other", "Other");
    s.handleEvent({ type: "registered", serverId: "other", nick: "other" });
    connectReady("other", false, 0, ["#chan"]);
    expect(api.join).not.toHaveBeenCalled();
    connectReady();
    expect(api.join).toHaveBeenCalledTimes(1);
    expect(api.join).toHaveBeenCalledWith(SID, "#chan");
  });

  it("remembers closed channel windows through interrupted reconnect attempts", () => {
    useSettings.getState().set("rejoinOnReconnect", true);
    useSettings.getState().set("keepOpenOnKickQuit", false);
    const s = useStore.getState();
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    s.handleEvent({ type: "join", serverId: SID, channel: "#chan", nick: "me" });
    s.handleEvent({ type: "disconnected", serverId: SID, reason: "x" });
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    s.handleEvent({ type: "disconnected", serverId: SID, reason: "before MOTD" });
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    connectReady(SID, false, 2);
    s.handleEvent({ type: "disconnected", serverId: SID, reason: "before delayed joins" });
    vi.advanceTimersByTime(2000);
    expect(api.join).not.toHaveBeenCalled();
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    connectReady();
    expect(api.join).toHaveBeenCalledTimes(1);
    expect(api.join).toHaveBeenCalledWith(SID, "#chan");
  });

  it("cancels delayed joins when the server closes", () => {
    useSettings.getState().set("rejoinOnReconnect", true);
    const s = useStore.getState();
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    s.handleEvent({ type: "join", serverId: SID, channel: "#chan", nick: "me" });
    s.handleEvent({ type: "disconnected", serverId: SID, reason: "x" });
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    connectReady(SID, false, 2);
    s.closeServer(SID);
    s.ensureServer(SID, "Replacement");
    s.handleEvent({ type: "registered", serverId: SID, nick: "new" });
    vi.advanceTimersByTime(2000);
    connectReady();
    expect(api.join).not.toHaveBeenCalled();
  });

  it("does not overflow a long autojoin delay into an immediate join", () => {
    useSettings.getState().set("rejoinOnReconnect", true);
    const s = useStore.getState();
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    s.handleEvent({ type: "join", serverId: SID, channel: "#chan", nick: "me" });
    s.handleEvent({ type: "disconnected", serverId: SID, reason: "x" });
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    connectReady(SID, false, 3000000);
    vi.advanceTimersByTime(2147483647);
    expect(api.join).not.toHaveBeenCalled();
    vi.advanceTimersByTime(3000000000 - 2147483647);
    expect(api.join).toHaveBeenCalledTimes(1);
    expect(api.join).toHaveBeenCalledWith(SID, "#chan");
  });

  it("rejoins a channel on self-kick only when the setting is on", () => {
    const s = useStore.getState();
    s.handleEvent({ type: "registered", serverId: SID, nick: "me" });
    s.handleEvent({ type: "join", serverId: SID, channel: "#chan", nick: "me" });

    useSettings.getState().set("rejoinOnKick", false);
    vi.mocked(api.join).mockClear();
    kickSelf("#chan");
    expect(api.join).not.toHaveBeenCalled();

    useSettings.getState().set("rejoinOnKick", true);
    kickSelf("#chan");
    expect(api.join).toHaveBeenCalledWith(SID, "#chan");
  });
});
