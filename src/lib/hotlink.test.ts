import { describe, expect, it } from "vitest";
import { planHotlinkMenu } from "./hotlink";

describe("planHotlinkMenu", () => {
  it("opens the window's own menu when no handler decided anything", () => {
    expect(planHotlinkMenu(null, "channel", "word")).toEqual({
      contexts: ["channel"],
      word: "",
      custom: "",
    });
    expect(
      planHotlinkMenu({ halted: false, menu: "", defaults: false }, "query", "word")
    ).toEqual({ contexts: ["query"], word: "", custom: "" });
  });

  it("opens no menu at all when the handler halted", () => {
    expect(
      planHotlinkMenu({ halted: true, menu: "", defaults: false }, "channel", "word")
    ).toEqual({ contexts: [], word: "", custom: "" });
    // A halt wins even if the same handler asked for a menu first.
    expect(
      planHotlinkMenu({ halted: true, menu: "@helppop", defaults: true }, "channel", "word")
    ).toEqual({ contexts: [], word: "", custom: "" });
  });

  it("replaces the menu for /hotlink -m, carrying the clicked word as $1", () => {
    expect(
      planHotlinkMenu({ halted: false, menu: "@helppop", defaults: false }, "channel", "/msg")
    ).toEqual({ contexts: ["@helppop"], word: "/msg", custom: "@helppop" });
  });

  it("appends the window's own popups for -d, after the custom menu", () => {
    expect(
      planHotlinkMenu({ halted: false, menu: "@helppop", defaults: true }, "channel", "/msg")
    ).toEqual({
      contexts: ["@helppop", "channel"],
      word: "/msg",
      custom: "@helppop",
    });
  });

  it("treats -d without -m as the window's own menu, since there is nothing to add to", () => {
    expect(
      planHotlinkMenu({ halted: false, menu: "", defaults: true }, "channel", "word")
    ).toEqual({ contexts: ["channel"], word: "", custom: "" });
  });
});
