import { beforeEach, describe, expect, it, vi } from "vitest";
import { parseEditboxCommand, routeClientCommand } from "./clientCommands";
import { useSettings } from "../state/settings";
import { STATUS, bufferKey, useStore } from "../state/store";

vi.mock("./api", () => ({
  api: {
    scriptWindowClose: vi.fn(() => Promise.resolve()),
    disconnect: vi.fn(() => Promise.resolve()),
  },
}));

describe("script client commands", () => {
  beforeEach(() => {
    localStorage.clear();
    useSettings.getState().set("layout", "tree");
    useSettings.getState().set("timestampMode", "inline");
    useStore.setState({
      servers: {},
      buffers: {},
      order: [],
      active: null,
      channelList: null,
      poppedOut: {},
    });
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

  it("routes timestamp and layout commands to persistent settings", () => {
    const base = { type: "clientCommand" as const, serverId: "s1", currentTarget: "#chat" };
    routeClientCommand({ ...base, command: "timestamp", args: "divider" }, vi.fn());
    expect(useSettings.getState().timestampMode).toBe("divider");
    routeClientCommand({ ...base, command: "switchbar", args: "on" }, vi.fn());
    expect(useSettings.getState().layout).toBe("switchbar");
    routeClientCommand({ ...base, command: "treebar", args: "on" }, vi.fn());
    expect(useSettings.getState().layout).toBe("tree");
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
});
