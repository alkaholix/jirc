import { describe, expect, it } from "vitest";
import { isQuietTime } from "./sound";

describe("quiet hours", () => {
  const at = (hour: number, minute = 0) => new Date(2026, 0, 1, hour, minute);

  it("handles a same-day range", () => {
    expect(isQuietTime(at(13), true, "12:00", "14:00")).toBe(true);
    expect(isQuietTime(at(15), true, "12:00", "14:00")).toBe(false);
  });

  it("handles a range crossing midnight", () => {
    expect(isQuietTime(at(23), true, "22:00", "07:00")).toBe(true);
    expect(isQuietTime(at(6), true, "22:00", "07:00")).toBe(true);
    expect(isQuietTime(at(12), true, "22:00", "07:00")).toBe(false);
  });
});
