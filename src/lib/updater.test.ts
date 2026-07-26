import { describe, expect, it } from "vitest";
import { updateNotificationBody, type UpdateStatus } from "./updater";

describe("updateNotificationBody", () => {
  it("notifies only when an update is available", () => {
    expect(
      updateNotificationBody({ state: "available", version: "26.7.82" }),
    ).toBe(
      "Version 26.7.82 is ready. Open Settings → Behaviour to review and install it.",
    );

    const silentStatuses: UpdateStatus[] = [
      { state: "idle" },
      { state: "checking" },
      { state: "current" },
      { state: "downloading", version: "26.7.82" },
      { state: "error", message: "offline" },
    ];
    for (const status of silentStatuses) {
      expect(updateNotificationBody(status)).toBeNull();
    }
  });
});
