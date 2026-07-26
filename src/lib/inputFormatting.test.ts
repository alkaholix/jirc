import { describe, expect, it } from "vitest";
import { colorControl, insertControl, IRC_FORMAT } from "./inputFormatting";

describe("input formatting controls", () => {
  it("inserts a toggle at the caret", () => {
    expect(insertControl("hello", 2, 2, IRC_FORMAT.bold)).toEqual({
      value: `he${IRC_FORMAT.bold}llo`,
      selectionStart: 3,
      selectionEnd: 3,
    });
  });

  it("wraps selected text and keeps it selected", () => {
    expect(insertControl("hello", 1, 4, IRC_FORMAT.underline)).toEqual({
      value: `h${IRC_FORMAT.underline}ell${IRC_FORMAT.underline}o`,
      selectionStart: 2,
      selectionEnd: 5,
    });
  });

  it("formats foreground and optional background colour numbers", () => {
    expect(colorControl(4)).toBe(`${IRC_FORMAT.color}04`);
    expect(colorControl(12, 1)).toBe(`${IRC_FORMAT.color}12,01`);
    expect(colorControl(120, -2)).toBe(`${IRC_FORMAT.color}99,00`);
  });
});
