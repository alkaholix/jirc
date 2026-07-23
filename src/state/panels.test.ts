import { beforeEach, describe, expect, it } from "vitest";
import { routePanelEvent, usePanels } from "./panels";

const event = (op: string, overrides: Record<string, string> = {}) =>
  ({
    type: "panel",
    serverId: "s1",
    op,
    panel: "stats",
    id: "",
    label: "",
    value: "",
    command: "",
    source: "ui.mrc",
    ...overrides,
  }) as const;

describe("script panel state", () => {
  beforeEach(() => usePanels.setState({ panels: [] }));

  it("owns typed text and button items", () => {
    routePanelEvent(event("upsert", { label: "Stats" }));
    routePanelEvent(event("text", { id: "users", value: "42 users" }));
    routePanelEvent(
      event("button", { id: "refresh", label: "Refresh", command: "/echo refresh" })
    );
    expect(usePanels.getState().panels[0]).toMatchObject({
      name: "stats",
      title: "Stats",
      items: [
        { id: "users", kind: "text", value: "42 users" },
        { id: "refresh", kind: "button", label: "Refresh" },
      ],
    });
  });

  it("deletes items, panels, and all panels", () => {
    routePanelEvent(event("upsert", { label: "Stats" }));
    routePanelEvent(event("text", { id: "users", value: "42 users" }));
    routePanelEvent(event("deleteItem", { id: "USERS" }));
    expect(usePanels.getState().panels[0].items).toEqual([]);
    routePanelEvent(event("deletePanel", { panel: "STATS" }));
    expect(usePanels.getState().panels).toEqual([]);
    routePanelEvent(event("upsert", { label: "Stats" }));
    routePanelEvent(event("clear"));
    expect(usePanels.getState().panels).toEqual([]);
  });
});
