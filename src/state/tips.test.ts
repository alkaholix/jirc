import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../lib/notify", () => ({ notify: vi.fn().mockResolvedValue(undefined) }));

import { activeTips, clearTips, routeTipCommand } from "./tips";

describe("scripted tips", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    clearTips();
  });

  it("creates, updates, closes, and expires named tips", () => {
    routeTipCommand(
      "tip-create",
      "counter\u001fCount Down\u001f10 seconds\u001f3\u001f\u001f\u001fclicked\u001f42",
      { serverId: "s1", target: "#test" }
    );
    expect(activeTips()).toMatchObject([{
      name: "counter", text: "10 seconds", alias: "clicked", wid: "42",
      serverId: "s1", target: "#test",
    }]);
    routeTipCommand("tip-update", "counter\u001f9 seconds");
    expect(activeTips()[0].text).toBe("9 seconds");
    vi.advanceTimersByTime(3000);
    expect(activeTips()).toEqual([]);

    routeTipCommand("tip-create", "notice\u001fNotice\u001fHello\u001f10");
    routeTipCommand("tip-close", "notice");
    expect(activeTips()).toEqual([]);
  });
});
