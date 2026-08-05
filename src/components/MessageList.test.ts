import { describe, expect, it } from "vitest";
import { hotlinkAtPoint, startsTimestampMinute } from "./MessageList";
import type { Line } from "../state/store";

const line = (ts: number): Line => ({
  id: ts,
  ts,
  kind: "msg",
  text: "message",
});

describe("hotlink pointer context", () => {
  it("finds the rendered word, line text, line number, and word position", () => {
    document.body.innerHTML = '<div class="msg-row" data-index="4"><span>one two hoverme four</span></div>';
    const node = document.querySelector("span")!.firstChild!;
    Object.defineProperty(document, "caretRangeFromPoint", {
      configurable: true,
      value: () => {
        const range = document.createRange();
        range.setStart(node, 10);
        return range;
      },
    });
    expect(hotlinkAtPoint(10, 10)).toEqual({
      word: "hoverme",
      fullLine: "one two hoverme four",
      line: 5,
      position: 3,
    });
  });
});

describe("timestamp minute dividers", () => {
  it("shows only on the first message and when the minute changes", () => {
    expect(startsTimestampMinute(line(60_000))).toBe(true);
    expect(startsTimestampMinute(line(89_999), line(60_000))).toBe(false);
    expect(startsTimestampMinute(line(120_000), line(89_999))).toBe(true);
  });
});
