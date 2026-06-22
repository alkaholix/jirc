import { describe, expect, it } from "vitest";
import { ircxBody, ircxDisplay } from "./ircx";

describe("ircxDisplay", () => {
  it("renders the 0x08 backspace byte as a space", () => {
    expect(ircxDisplay("%#The\bLobby")).toBe("%#The Lobby");
    expect(ircxDisplay("Welcome\bto\bThe\bLobby")).toBe("Welcome to The Lobby");
  });

  it("renders a LITERAL backslash-b as a space (IRC7 encoding)", () => {
    expect(ircxDisplay("%#The\\bLobby")).toBe("%#The Lobby");
    expect(ircxDisplay("a\\bb\\bc")).toBe("a b c");
  });

  it("leaves normal names and undefined untouched", () => {
    expect(ircxDisplay("#channel")).toBe("#channel");
    expect(ircxDisplay(undefined)).toBe("");
  });
});

describe("ircxBody", () => {
  it("decodes 0x08 but NOT a literal backslash-b (keeps chat text intact)", () => {
    expect(ircxBody("a\bb")).toBe("a b");
    expect(ircxBody("C:\\backup")).toBe("C:\\backup");
  });
});
