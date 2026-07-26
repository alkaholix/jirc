import { describe, expect, it } from "vitest";
import { normalizeSavedSettings } from "./settings";

describe("timestamp settings migration", () => {
  it("maps the legacy timestamp toggle and preserves an explicit new mode", () => {
    expect(normalizeSavedSettings({ showTimestamps: false }).timestampMode).toBe("off");
    expect(normalizeSavedSettings({ showTimestamps: true }).timestampMode).toBe("inline");
    expect(normalizeSavedSettings({ timestampMode: "divider" }).timestampMode).toBe("divider");
  });

  it("enables the formatting toolbar for existing saved settings", () => {
    expect(normalizeSavedSettings({}).showInputToolbar).toBe(true);
    expect(normalizeSavedSettings({ showInputToolbar: false }).showInputToolbar).toBe(false);
  });
});
