import { describe, expect, it } from "vitest";
import { moveDockPane } from "./DockLayout";

describe("dock pane ordering", () => {
  it("moves a pane before another pane or to the end", () => {
    expect(moveDockPane(["treebar", "nicklist", "panels"], "panels", "treebar"))
      .toEqual(["panels", "treebar", "nicklist"]);
    expect(moveDockPane(["panels", "treebar", "nicklist"], "panels"))
      .toEqual(["treebar", "nicklist", "panels"]);
  });
});
