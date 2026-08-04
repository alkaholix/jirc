import { beforeEach, describe, expect, it, vi } from "vitest";
import { FINDTEXT_COMMAND_EVENT, parseEditboxCommand, routeClientCommand } from "./clientCommands";
import { useSettings } from "../state/settings";
import { STATUS, bufferKey, useStore } from "../state/store";
import { useToolbar } from "../state/toolbar";
import { useChannelCentral } from "../state/channelModes";
import { useAddressBook } from "../state/addressBook";

vi.mock("./api", () => ({
  api: {
    scriptWindowClose: vi.fn(() => Promise.resolve()),
    disconnect: vi.fn(() => Promise.resolve()),
    openHelp: vi.fn(() => Promise.resolve()),
    sendRaw: vi.fn(() => Promise.resolve()),
    sendMessage: vi.fn(() => Promise.resolve()),
  },
}));

describe("script client commands", () => {
  beforeEach(() => {
    localStorage.clear();
    useSettings.getState().set("layout", "tree");
    useSettings.getState().set("timestampMode", "inline");
    useSettings.getState().set("stripCodes", "");
    useToolbar.getState().setVisible(true);
    useChannelCentral.getState().close();
    useAddressBook.setState({ entries: [], loaded: true, error: "", open: false, requestedNick: "", requestedNetwork: "" });
    useStore.setState({
      servers: {},
      buffers: {},
      order: [],
      active: null,
      channelList: null,
      poppedOut: {},
    });
  });

  it("opens the address book from a script command", () => {
    useStore.setState({ servers: { s1: { id: "s1", name: "IRC7" } as never } });
    routeClientCommand({
      type: "clientCommand",
      serverId: "s1",
      command: "abook",
      args: "-w Alice",
      currentTarget: "#chat",
    }, vi.fn());
    expect(useAddressBook.getState()).toMatchObject({
      open: true,
      requestedNick: "Alice",
      requestedNetwork: "IRC7",
    });
  });

  it("opens Channel Central for script and popup channel commands", () => {
    routeClientCommand({
      type: "clientCommand",
      serverId: "s1",
      command: "channel",
      args: "#other",
      currentTarget: "#chat",
    }, vi.fn());
    expect(useChannelCentral.getState().target).toEqual({ serverId: "s1", channel: "#other" });
  });

  it("parses editbox targets, submission, spacing and selection", () => {
    expect(parseEditboxCommand("s1", "#chat", "-af1pb2e5 hello world")).toEqual({
      serverId: "s1",
      target: "#chat",
      text: "hello world",
      appendSpace: true,
      submit: false,
      focus: true,
      selectionStart: 2,
      selectionEnd: 5,
    });
    expect(parseEditboxCommand("s1", "#chat", "-sn status text").target).toBe(STATUS);
    expect(parseEditboxCommand("s1", "#chat", "-sn status text").submit).toBe(true);
    expect(parseEditboxCommand("s1", "#chat", "hello world")).toMatchObject({
      target: "#chat",
      text: "hello world",
    });
    expect(parseEditboxCommand("s1", "#chat", "#other hello")).toMatchObject({
      target: "#other",
      text: "hello",
    });
  });

  it("routes findtext into the current buffer search", () => {
    const listener = vi.fn();
    window.addEventListener(FINDTEXT_COMMAND_EVENT, listener);
    routeClientCommand({
      type: "clientCommand",
      serverId: "s1",
      command: "findtext",
      args: "-n hello world",
      currentTarget: "#chat",
    }, vi.fn());
    expect((listener.mock.calls[0][0] as CustomEvent).detail).toEqual({
      serverId: "s1",
      target: "#chat",
      text: "hello world",
      next: true,
    });
    window.removeEventListener(FINDTEXT_COMMAND_EVENT, listener);
  });

  it("routes timestamp and layout commands to persistent settings", () => {
    const base = { type: "clientCommand" as const, serverId: "s1", currentTarget: "#chat" };
    routeClientCommand({ ...base, command: "timestamp", args: "divider" }, vi.fn());
    expect(useSettings.getState().timestampMode).toBe("divider");
    routeClientCommand({ ...base, command: "switchbar", args: "on" }, vi.fn());
    expect(useSettings.getState().layout).toBe("switchbar");
    routeClientCommand({ ...base, command: "treebar", args: "on" }, vi.fn());
    expect(useSettings.getState().layout).toBe("tree");
    routeClientCommand({ ...base, command: "toolbar", args: "off" }, vi.fn());
    expect(useToolbar.getState().visible).toBe(false);
  });

  it("applies the application font with an 8px minimum", () => {
    const base = { type: "clientCommand" as const, serverId: "s1", currentTarget: "#chat" };
    routeClientCommand({ ...base, command: "font", args: "5 Arial" }, vi.fn());
    expect(useSettings.getState().chatFontSize).toBe(8);
    expect(useSettings.getState().chatFont).toBe("Arial");
    routeClientCommand({ ...base, command: "font", args: "-z" }, vi.fn());
    expect(useSettings.getState().chatFontSize).toBe(0);
    expect(useSettings.getState().chatFont).toBe("");
  });

  it("clears only the requested buffer types", () => {
    const channel = bufferKey("s1", "#chat");
    const query = bufferKey("s1", "nick");
    const line = { id: 1, ts: 1, kind: "system" as const, text: "line" };
    useStore.setState({
      buffers: {
        [channel]: {
          key: channel, serverId: "s1", name: "#chat", kind: "channel",
          lines: [line], members: [], unread: 0, mention: false,
        },
        [query]: {
          key: query, serverId: "s1", name: "nick", kind: "query",
          lines: [line], members: [], unread: 0, mention: false,
        },
      },
      order: [channel, query],
    });
    routeClientCommand({
      type: "clientCommand",
      serverId: "s1",
      command: "clearall",
      args: "-n",
      currentTarget: "#chat",
    }, vi.fn());
    expect(useStore.getState().buffers[channel].lines).toEqual([]);
    expect(useStore.getState().buffers[query].lines).toHaveLength(1);
  });

  it("routes query rename and per-buffer logging", () => {
    const query = bufferKey("s1", "oldnick");
    useStore.setState({
      buffers: {
        [query]: {
          key: query, serverId: "s1", name: "oldnick", kind: "query",
          lines: [], members: [], unread: 0, mention: false,
        },
      },
      order: [query],
    });
    const base = { type: "clientCommand" as const, serverId: "s1", currentTarget: "oldnick" };
    routeClientCommand({ ...base, command: "log", args: "off oldnick" }, vi.fn());
    expect(useStore.getState().buffers[query].logging).toBe(false);
    routeClientCommand({ ...base, command: "queryrn", args: "oldnick newnick" }, vi.fn());
    expect(Object.values(useStore.getState().buffers).some((buffer) => buffer.name === "newnick")).toBe(true);
  });

  it("marks one or every connection buffer as read", () => {
    const one = bufferKey("s1", "#one");
    const two = bufferKey("s1", "#two");
    useStore.setState({
      buffers: {
        [one]: { key: one, serverId: "s1", name: "#one", kind: "channel", lines: [], members: [], unread: 2, mention: true },
        [two]: { key: two, serverId: "s1", name: "#two", kind: "channel", lines: [], members: [], unread: 3, mention: true },
      }, order: [one, two],
    });
    const base = { type: "clientCommand" as const, serverId: "s1", currentTarget: "#one" };
    routeClientCommand({ ...base, command: "markasread", args: "#one" }, vi.fn());
    expect(useStore.getState().buffers[one]).toMatchObject({ unread: 0, mention: false });
    expect(useStore.getState().buffers[two].unread).toBe(3);
    routeClientCommand({ ...base, command: "markasread", args: "" }, vi.fn());
    expect(useStore.getState().buffers[two]).toMatchObject({ unread: 0, mention: false });
  });

  it("updates individual strip flags", () => {
    const base = { type: "clientCommand" as const, serverId: "s1", currentTarget: "#one" };
    routeClientCommand({ ...base, command: "strip", args: "+bur-c" }, vi.fn());
    expect(useSettings.getState().stripCodes).toBe("bur");
    routeClientCommand({ ...base, command: "strip", args: "-u+i" }, vi.fn());
    expect(useSettings.getState().stripCodes).toBe("bri");
  });
});
