import { describe, expect, it } from "vitest";
import { parsePerformCommands } from "./ConnectDialog";

describe("Perform-on-connect commands", () => {
  it("preserves command order while removing blank lines and outer whitespace", () => {
    expect(
      parsePerformCommands(" /mode $me +i \r\n\nmsg NickServ STATUS\n /join #staff ")
    ).toEqual([
      "/mode $me +i",
      "msg NickServ STATUS",
      "/join #staff",
    ]);
  });
});
