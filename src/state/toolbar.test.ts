import { beforeEach, describe, expect, it } from "vitest";
import { routeToolbarEvent, useToolbar } from "./toolbar";

const event = (op: string, overrides: Record<string, string> = {}) =>
  ({
    type: "toolbar",
    serverId: "s1",
    op,
    name: "Cow",
    tooltip: "",
    icon: "",
    command: "",
    source: "tools.mrc",
    ...overrides,
  }) as const;

describe("script toolbar state", () => {
  beforeEach(() => useToolbar.setState({ buttons: [] }));

  it("adds, updates, and removes buttons case-insensitively", () => {
    routeToolbarEvent(
      event("upsert", { tooltip: "Moo", icon: "🐄", command: "/echo moo" })
    );
    routeToolbarEvent(event("tooltip", { name: "cow", tooltip: "New tip" }));
    expect(useToolbar.getState().buttons).toEqual([
      {
        name: "cow",
        tooltip: "New tip",
        icon: "🐄",
        command: "/echo moo",
        source: "tools.mrc",
        serverId: "s1",
      },
    ]);
    routeToolbarEvent(event("delete", { name: "COW" }));
    expect(useToolbar.getState().buttons).toEqual([]);
  });

  it("clears every script button", () => {
    routeToolbarEvent(event("upsert", { command: "/echo moo" }));
    routeToolbarEvent(event("clear"));
    expect(useToolbar.getState().buttons).toEqual([]);
  });
});
