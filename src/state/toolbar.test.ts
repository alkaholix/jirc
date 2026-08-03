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
  beforeEach(() => useToolbar.setState({ buttons: [], visible: true }));

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
        enabled: true,
        visible: true,
        checked: false,
        separator: false,
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

  it("updates enabled, visible, checked, and separator state", () => {
    routeToolbarEvent(event("upsert", { command: "/echo moo" }));
    routeToolbarEvent(event("enabled", { command: "0" }));
    routeToolbarEvent(event("visible", { command: "0" }));
    routeToolbarEvent(event("checked", { command: "1" }));
    routeToolbarEvent(event("separator", { name: "sep" }));
    expect(useToolbar.getState().buttons[0]).toMatchObject({ enabled: false, visible: false, checked: true });
    expect(useToolbar.getState().buttons[1]).toMatchObject({ separator: true });
  });
});
