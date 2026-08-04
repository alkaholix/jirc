import { describe, expect, it } from "vitest";
import { normalizeAppFontSize, normalizeSavedSettings } from "./settings";

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

  it("keeps the application menubar and scripted tips enabled by default", () => {
    expect(normalizeSavedSettings({})).toMatchObject({ menubarVisible: true, tipsEnabled: true, nativePopupMenus: false });
    expect(normalizeSavedSettings({ menubarVisible: false, tipsEnabled: false })).toMatchObject({
      menubarVisible: false,
      tipsEnabled: false,
    });
  });

  it("enables native spell checking for existing settings and preserves language choices", () => {
    expect(normalizeSavedSettings({}).spellCheck).toBe(true);
    expect(normalizeSavedSettings({}).autoCorrect).toBe(false);
    expect(normalizeSavedSettings({}).spellCheckLanguage).toBe("");
    expect(
      normalizeSavedSettings({
        spellCheck: false,
        spellCheckLanguage: "en-NZ",
      })
    ).toMatchObject({
      spellCheck: false,
      spellCheckLanguage: "en-NZ",
    });
  });

  it("enforces an 8px minimum for custom application font sizes", () => {
    expect(normalizeAppFontSize(7)).toBe(8);
    expect(normalizeAppFontSize(-6)).toBe(8);
    expect(normalizeAppFontSize(12)).toBe(12);
    expect(normalizeAppFontSize(0)).toBe(0);
    expect(normalizeSavedSettings({ chatFontSize: 5 }).chatFontSize).toBe(8);
  });

  it("migrates and validates dockable pane layout settings", () => {
    expect(normalizeSavedSettings({ treebarPosition: "right" })).toMatchObject({
      dockPaneOrder: ["treebar", "nicklist", "panels"],
      dockPaneSides: { treebar: "right", nicklist: "right", panels: "right" },
      nicklistWidth: 180,
      panelsWidth: 240,
    });
    expect(normalizeSavedSettings({
      dockPaneOrder: ["panels", "bad", "panels"],
      dockPaneSides: { panels: "left" },
      nicklistWidth: 20,
    })).toMatchObject({
      dockPaneOrder: ["panels", "treebar", "nicklist"],
      dockPaneSides: { treebar: "left", nicklist: "right", panels: "left" },
      nicklistWidth: 120,
    });
  });
});
