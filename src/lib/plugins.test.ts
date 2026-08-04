import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./api", () => ({ api: {
  scriptWindowOpen: vi.fn().mockResolvedValue(undefined),
  logRead: vi.fn().mockResolvedValue(""),
  logAppend: vi.fn().mockResolvedValue(undefined),
} }));
vi.mock("./notify", () => ({ notify: vi.fn().mockResolvedValue(undefined) }));

import { applyPluginDispatch } from "./plugins";
import { useStore, type Buffer } from "../state/store";

const buffer = { serverId: "s1", name: "#test", kind: "channel" } as Buffer;

beforeEach(() => {
  useStore.setState({ servers: {}, buffers: {}, order: [], active: null });
});

describe("plugin capability routing", () => {
  it("routes validated echo and command actions through jIRC", async () => {
    const runCommand = vi.fn().mockResolvedValue(undefined);
    await applyPluginDispatch({
      handled: true,
      errors: [],
      actions: [
        { type: "echo", target: "#test", text: "hello" },
        { type: "command", command: "/join #plugins" },
      ],
    }, buffer, runCommand);

    const line = Object.values(useStore.getState().buffers)[0].lines[0];
    expect(line.text).toBe("hello");
    expect(runCommand).toHaveBeenCalledWith("/join #plugins");
  });
});
