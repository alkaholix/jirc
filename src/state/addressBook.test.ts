import { describe, expect, it } from "vitest";
import { normalizeAddressEntries } from "./addressBook";

describe("address book persistence", () => {
  it("keeps valid entries, migrates missing fields, and rejects empty nicks", () => {
    expect(normalizeAddressEntries([
      { id: "1", nick: " Alice ", notes: "friend" },
      { nick: "   " },
      null,
    ])).toEqual([{
      id: "1",
      nick: "Alice",
      network: "",
      name: "",
      email: "",
      website: "",
      notes: "friend",
    }]);
  });
});
