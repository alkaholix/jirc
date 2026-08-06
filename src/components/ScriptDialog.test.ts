import { beforeEach, describe, expect, it } from "vitest";
import { POPUP_SECTIONS, clearDraft, liveDraft, migrateLegacyDrafts, saveDraft } from "./ScriptDialog";
void clearDraft;

// The default popup menus exist twice: as .msl files the Rust side seeds on
// first run, and as templates the editor falls back to when a file is missing.
// They must stay identical, or the editor would show a user something different
// from what jIRC actually wrote to disk.
//
// Imported with Vite's `?raw` so this needs no Node types in the app tsconfig.
import statusMsl from "../../src-tauri/src/script/examples/popups-status.msl?raw";
import channelMsl from "../../src-tauri/src/script/examples/popups-channel.msl?raw";
import nicklistMsl from "../../src-tauri/src/script/examples/popups-nicklist.msl?raw";
import queryMsl from "../../src-tauri/src/script/examples/popups-query.msl?raw";

const SEEDED: Record<string, string> = {
  "popups-status": statusMsl,
  "popups-channel": channelMsl,
  "popups-nicklist": nicklistMsl,
  "popups-query": queryMsl,
};

const normalise = (s: string) => s.replace(/\r\n/g, "\n");

describe("default popup menus", () => {
  it.each(Object.keys(SEEDED))("%s matches the file the backend seeds", (name) => {
    const section = POPUP_SECTIONS.find((s) => s.name === name);
    expect(section, `no editor section named ${name}`).toBeDefined();
    expect(normalise(section!.template)).toBe(normalise(SEEDED[name]));
  });

  it("keeps the combined file free of menus so contexts are not duplicated", () => {
    const combined = POPUP_SECTIONS.find((s) => s.id === "combined");
    expect(combined?.template).not.toMatch(/^\s*menu\s/m);
  });
});

// A cached draft used to win over the file on disk unconditionally, so a
// reseeded default was invisible and there was no way to clear it. These pin
// the rules that stop that recurring.
describe("script drafts", () => {
  beforeEach(() => localStorage.clear());

  it("shows a draft that is genuine unsaved work", () => {
    saveDraft("x", "edited", "on disk");
    expect(liveDraft("x", "on disk")).toBe("edited");
  });

  it("drops a draft once the file changes underneath it", () => {
    saveDraft("x", "edited", "old file");
    expect(liveDraft("x", "NEW file")).toBeNull();
    expect(localStorage.getItem("jirc.script-draft.x")).toBeNull();
  });

  it("drops a draft that matches the file", () => {
    saveDraft("x", "same", "same");
    expect(liveDraft("x", "same")).toBeNull();
  });

  it("clears legacy drafts with no recorded base, exactly once", () => {
    localStorage.setItem("jirc.script-draft.old", "stale");
    saveDraft("tracked", "edited", "base");
    migrateLegacyDrafts();
    expect(localStorage.getItem("jirc.script-draft.old")).toBeNull();
    expect(localStorage.getItem("jirc.script-draft.tracked")).toBe("edited");
    localStorage.setItem("jirc.script-draft.later", "kept");
    migrateLegacyDrafts();
    expect(localStorage.getItem("jirc.script-draft.later")).toBe("kept");
  });
});
