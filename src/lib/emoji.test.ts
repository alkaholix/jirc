import { describe, it, expect } from "vitest";
import { expandEmoji, imageEmoji } from "./emoji";
import { useSettings } from "../state/settings";

describe("expandEmoji", () => {
  it("replaces known shortcodes", () => {
    expect(expandEmoji("hello :smile:")).toBe("hello 😄");
    expect(expandEmoji(":+1: :fire:")).toBe("👍 🔥");
  });

  it("leaves unknown shortcodes intact", () => {
    expect(expandEmoji("a :notanemoji: b")).toBe("a :notanemoji: b");
  });

  it("is case-insensitive", () => {
    expect(expandEmoji(":SMILE:")).toBe("😄");
  });

  it("supports custom text emoji and leaves image emoji as codes", () => {
    useSettings.getState().set("customEmoji", {
      ":shrug:": "¯\\_(ツ)_/¯",
      ":doge:": "https://x/doge.png",
    });
    expect(expandEmoji("a :shrug: b")).toBe("a ¯\\_(ツ)_/¯ b");
    expect(expandEmoji("a :doge: b")).toBe("a :doge: b");
    expect(imageEmoji()[":doge:"]).toBe("https://x/doge.png");
    useSettings.getState().set("customEmoji", {});
  });
});
