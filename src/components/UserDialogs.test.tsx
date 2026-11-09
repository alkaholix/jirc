// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api, type DialogControl } from "../lib/api";
import { routeDialogEvent, useDialogs } from "../state/dialogs";
import { UserDialogs } from "./UserDialogs";

vi.mock("../lib/api", () => ({
  api: {
    scriptRunDialog: vi.fn(() => Promise.resolve(false)),
    scriptPictureRead: vi.fn(() => Promise.resolve("data:image/png;base64,aWNvbg==")),
  },
}));
vi.mock("../state/store", () => ({
  useStore: (selector: (state: { servers: Record<string, { nick: string; name: string }> }) => unknown) =>
    selector({ servers: { s: { nick: "me", name: "Network" } } }),
}));

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean })
  .IS_REACT_ACT_ENVIRONMENT = true;

const control = (kind: string, id: string, label = "", extra: Partial<DialogControl> = {}): DialogControl => ({
  kind, id, label, options: [], default: false, cancel: false, ok: false,
  styles: [], enabled: true, visible: true, tab: "", ...extra,
});

describe("live script dialog controls", () => {
  let host: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    vi.clearAllMocks();
    useDialogs.setState({ dialogs: [] });
    host = document.createElement("div");
    document.body.appendChild(host);
    root = createRoot(host);
  });

  afterEach(async () => {
    await act(async () => root.unmount());
    host.remove();
  });

  const open = (controls: DialogControl[], name = "settings") => routeDialogEvent({
    type: "dialogOpen", serverId: "s", name, title: name, controls, width: 420, height: 300,
  });
  const mutate = (id: string, op: string, value = "", dialog = "settings") => routeDialogEvent({
    type: "dialogSet", serverId: "s", dialog, control: id, op, value,
  });
  const element = <T extends HTMLElement>(id: string) => host.querySelector<T>(`[data-dialog-control="${id}"]`)!;
  const lastSnapshot = () => vi.mocked(api.scriptRunDialog).mock.calls.at(-1)![6];

  it("renders appended and replaced text, captions, check state, and icons", async () => {
    open([
      control("text", "1", "Status"), control("edit", "2", "hello"),
      control("editbox", "3", "line one"), control("button", "4", "Old button"),
      control("check", "5", "Old choice"), control("box", "6", "Old group"),
      control("link", "7", "https://old.example"), control("icon", "8", "old.png"),
    ]);
    await act(async () => root.render(<UserDialogs />));
    await act(async () => {
      mutate("1", "add", ": ready");
      mutate("2", "add", " there");
      mutate("3", "add", "\nline two");
      mutate("4", "clear");
      mutate("4", "add", "New button");
      mutate("5", "indeterminate");
      mutate("5", "set", "New choice");
      mutate("6", "set", "New group");
      mutate("7", "set", "https://new.example");
      mutate("8", "icon", "new.png");
    });
    expect(host.querySelector(".dlg-text")?.textContent).toBe("Status: ready");
    expect(element<HTMLInputElement>("2").value).toBe("hello there");
    expect(element<HTMLTextAreaElement>("3").value).toBe("line one\nline two");
    expect(element<HTMLButtonElement>("4").textContent).toBe("New button");
    expect(element<HTMLInputElement>("5").checked).toBe(true);
    expect(element<HTMLInputElement>("5").indeterminate).toBe(true);
    expect(element("5").parentElement?.textContent).toBe("New choice");
    expect(host.querySelector("legend")?.textContent).toBe("New group");
    expect(host.querySelector("a")?.getAttribute("href")).toBe("https://new.example");
    expect(api.scriptPictureRead).toHaveBeenLastCalledWith("new.png");
    expect(host.querySelector("img")).not.toBeNull();

    await act(async () => {
      mutate("1", "clear");
      mutate("2", "clear");
      mutate("2", "add", "replacement");
      mutate("3", "clear");
    });
    expect(host.querySelector(".dlg-text")?.textContent).toBe("");
    expect(element<HTMLInputElement>("2").value).toBe("replacement");
    expect(element<HTMLTextAreaElement>("3").value).toBe("");
  });

  it("selects duplicate options by index and reports the new indices and text in the same event", async () => {
    open([
      control("list", "1", "", { options: ["same", "same", "last"], styles: ["multsel"] }),
      control("combo", "2", "", { options: ["same", "same", "last"] }),
      control("list", "3"),
    ]);
    await act(async () => root.render(<UserDialogs />));
    await act(async () => {
      mutate("1", "check", "2");
      mutate("1", "check-keep", "3");
      mutate("2", "check", "2");
      mutate("3", "add", "runtime option");
    });
    const list = element<HTMLSelectElement>("1");
    const combo = element<HTMLSelectElement>("2");
    expect(Array.from(list.selectedOptions).map((option) => option.value)).toEqual(["2", "3"]);
    expect(combo.value).toBe("2");
    expect(element<HTMLSelectElement>("3").selectedIndex).toBe(-1);

    await act(async () => {
      list.options[2].selected = false;
      list.dispatchEvent(new Event("change", { bubbles: true }));
    });
    expect(lastSnapshot()).toMatchObject({
      "1": "same", "\u0000selected\u00001": "2", "\u0000options\u00001": "same\nsame\nlast",
      "\u0000edited\u00001": "true", "\u0000selected\u00002": "2",
    });
    expect(useDialogs.getState().dialogs[0].selected["1"]).toEqual([2]);
    await act(async () => {
      combo.value = "3";
      combo.dispatchEvent(new Event("change", { bubbles: true }));
    });
    expect(lastSnapshot()).toMatchObject({ "2": "last", "\u0000selected\u00002": "3" });
    expect(vi.mocked(api.scriptRunDialog).mock.calls.at(-1)?.slice(3, 6)).toEqual(["settings", "sclick", "2"]);
  });

  it("switches and focuses tabs, then renames and activates without reopening or losing edits", async () => {
    open([
      control("tab", "1", "First"), control("tab", "2", "Second"),
      control("edit", "3", "first", { tab: "1" }), control("edit", "4", "second", { tab: "2" }),
    ]);
    await act(async () => root.render(<UserDialogs />));
    await act(async () => mutate("2", "tab"));
    expect(element<HTMLButtonElement>("2").getAttribute("aria-selected")).toBe("true");
    expect(document.activeElement).toBe(element("2"));
    expect(host.querySelector('[data-dialog-control="3"]')).toBeNull();
    const input = element<HTMLInputElement>("4");
    await act(async () => useDialogs.getState().setValue("SETTINGS", "4", "typed"));
    await act(async () => mutate("", "rename", "renamed"));
    expect(element("4")).toBe(input);
    expect(input.value).toBe("typed");
    expect(vi.mocked(api.scriptRunDialog).mock.calls.filter((call) => call[4] === "init")).toHaveLength(1);

    await act(async () => open([], "other"));
    await act(async () => mutate("", "activate", "", "RENAMED"));
    expect(Array.from(host.querySelectorAll("h2")).map((heading) => heading.textContent)).toEqual(["other", "settings"]);
    expect(element("4")).toBe(input);
    expect(input.value).toBe("typed");
    expect(vi.mocked(api.scriptRunDialog).mock.calls.filter((call) => call[4] === "init")).toHaveLength(2);
  });
});
