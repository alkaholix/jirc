import { describe, expect, it } from "vitest";
import { shouldShowDefaultNicklistMenu } from "./NickList";

describe("nick-list popup fallback", () => {
  it("uses the native menu only when scripts provide no nick-list entries", () => {
    expect(shouldShowDefaultNicklistMenu([])).toBe(true);
    expect(shouldShowDefaultNicklistMenu([{
      label: "Whois",
      command: "/whois $1",
      separator: false,
      children: [],
    }])).toBe(false);
  });
});
