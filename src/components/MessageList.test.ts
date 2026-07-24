import { describe, expect, it } from "vitest";
import { startsTimestampMinute } from "./MessageList";
import type { Line } from "../state/store";

const line = (ts: number): Line => ({
  id: ts,
  ts,
  kind: "msg",
  text: "message",
});

describe("timestamp minute dividers", () => {
  it("shows only on the first message and when the minute changes", () => {
    expect(startsTimestampMinute(line(60_000))).toBe(true);
    expect(startsTimestampMinute(line(89_999), line(60_000))).toBe(false);
    expect(startsTimestampMinute(line(120_000), line(89_999))).toBe(true);
  });
});
