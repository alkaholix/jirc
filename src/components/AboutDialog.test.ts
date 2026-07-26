import { describe, expect, it } from "vitest";
import { formatCoreVersion } from "./AboutDialog";

describe("AboutDialog version", () => {
  it("shows the application version without the backend label", () => {
    expect(formatCoreVersion("jIRC core 26.7.79")).toBe("26.7.79");
  });
});
