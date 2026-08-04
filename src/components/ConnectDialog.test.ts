// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { describe, expect, it, vi } from "vitest";
import { ConnectDialog, parsePerformCommands } from "./ConnectDialog";

vi.mock("../lib/api", () => ({
  api: {
    profilesLoad: vi.fn(() => Promise.resolve([])),
    profilesSave: vi.fn(() => Promise.resolve()),
  },
}));

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean })
  .IS_REACT_ACT_ENVIRONMENT = true;

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

  it("does not close when its backdrop is clicked", async () => {
    const host = document.createElement("div");
    document.body.appendChild(host);
    const root = createRoot(host);
    const onClose = vi.fn();

    await act(async () => {
      root.render(createElement(ConnectDialog, { onClose, onConnect: vi.fn() }));
    });
    const backdrop = host.querySelector<HTMLElement>(".modal-backdrop");
    expect(backdrop).not.toBeNull();
    act(() => backdrop!.click());
    expect(onClose).not.toHaveBeenCalled();

    act(() => root.unmount());
    host.remove();
  });
});
