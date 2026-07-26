import { describe, expect, it } from "vitest";
import { replaceInputSelection, spellCheckAttributes } from "./InputBar";

describe("message input spell checking", () => {
  it("follows the enabled setting and omits a language for the system default", () => {
    expect(spellCheckAttributes(true, "")).toEqual({
      spellCheck: true,
      lang: undefined,
    });
    expect(spellCheckAttributes(false, "en-NZ")).toEqual({
      spellCheck: false,
      lang: "en-NZ",
    });
  });
});

describe("message input context-menu editing", () => {
  it("inserts emoji and pasted text at the current selection", () => {
    expect(replaceInputSelection("hello wrld", 6, 10, "world")).toEqual({
      value: "hello world",
      caret: 11,
    });
    expect(replaceInputSelection("hello ", 6, 6, "😀")).toEqual({
      value: "hello 😀",
      caret: 8,
    });
  });
});
