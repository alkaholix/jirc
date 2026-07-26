import { describe, expect, it } from "vitest";
import { spellCheckAttributes } from "./InputBar";

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
