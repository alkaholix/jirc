import { beforeEach, describe, expect, it } from "vitest";
import { routeDialogEvent, useDialogs } from "./dialogs";
import type { DialogControl } from "../lib/api";

const controls: DialogControl[] = [
  {
    kind: "edit", id: "10", label: "hello", options: [], default: false,
    cancel: false, ok: false, styles: [], enabled: true, visible: true, tab: "",
  },
  {
    kind: "list", id: "11", label: "", options: ["one", "two"], default: false,
    cancel: false, ok: false, styles: ["multsel"], enabled: true, visible: true, tab: "",
  },
  {
    kind: "button", id: "20", label: "Save", options: [], default: true,
    cancel: false, ok: true, styles: ["default", "ok"], enabled: true, visible: true, tab: "",
  },
];

const control = (kind: string, id: string, label = "", extra: Partial<DialogControl> = {}): DialogControl => ({
  kind, id, label, options: [], default: false, cancel: false, ok: false,
  styles: [], enabled: true, visible: true, tab: "", ...extra,
});

const open = (items: DialogControl[], name = "settings") => routeDialogEvent({
  type: "dialogOpen", serverId: "s", name, title: "Settings", controls: items, width: 420, height: 300,
});
const mutate = (id: string, op: string, value = "", dialog = "settings") => routeDialogEvent({
  type: "dialogSet", serverId: "s", dialog, control: id, op, value,
});
const current = () => useDialogs.getState().dialogs[0];

describe("script dialog state", () => {
  beforeEach(() => useDialogs.setState({ dialogs: [] }));

  it("opens with metadata and applies portable /did operations", () => {
    routeDialogEvent({
      type: "dialogOpen", serverId: "s", name: "settings", title: "Settings",
      controls, width: 420, height: 300,
    });
    expect(useDialogs.getState().dialogs[0]).toMatchObject({
      name: "settings", width: 420, height: 300,
      values: { "10": "hello", "11": "one" },
    });

    routeDialogEvent({ type: "dialogSet", serverId: "s", dialog: "settings", control: "11", op: "insert", value: "2 middle" });
    routeDialogEvent({ type: "dialogSet", serverId: "s", dialog: "settings", control: "10", op: "disable", value: "" });
    routeDialogEvent({ type: "dialogSet", serverId: "s", dialog: "settings", control: "20", op: "hide", value: "" });
    routeDialogEvent({ type: "dialogSet", serverId: "s", dialog: "settings", control: "", op: "title", value: "Changed" });

    const dialog = useDialogs.getState().dialogs[0];
    expect(dialog.options["11"]).toEqual(["one", "middle", "two"]);
    expect(dialog.controls.find((control) => control.id === "10")?.enabled).toBe(false);
    expect(dialog.controls.find((control) => control.id === "20")?.visible).toBe(false);
    expect(dialog.title).toBe("Changed");
  });

  it("appends and resets live text and captions without changing check state", () => {
    open([
      control("text", "1", "Status"), control("edit", "2", "hello"),
      control("button", "3", "Save"), control("check", "4", "Choice"),
      control("icon", "5", "old.png"), control("editbox", "6", "line one"),
    ]);
    expect(current().values["1"]).toBe("Status");
    mutate("1", "add", ": ready");
    useDialogs.getState().setValue("settings", "2", "typed");
    mutate("2", "add", " text");
    mutate("6", "add", "\nline two");
    mutate("4", "indeterminate");
    mutate("4", "add", " changed");
    mutate("5", "icon", "new.png");
    expect(current().values).toMatchObject({ "1": "Status: ready", "2": "typed text", "4": "2", "6": "line one\nline two" });
    expect(current().controls.find((item) => item.id === "4")?.label).toBe("Choice changed");
    expect(current().controls.find((item) => item.id === "5")?.label).toBe("new.png");

    for (const id of ["1", "2", "3", "4", "6"]) {
      mutate(id, "clear");
      mutate(id, "add", "replacement");
    }
    expect(current().values).toMatchObject({ "1": "replacement", "2": "replacement", "3": "replacement", "4": "2", "6": "replacement" });
    expect(current().controls.find((item) => item.id === "4")?.label).toBe("replacement");
    expect(current().options["2"]).toEqual([]);
    mutate("2", "check", "1 3");
    expect(current().values["2"]).toBe("replacement");
  });

  it("tracks duplicate list items by index and preserves other selections only when requested", () => {
    open([
      control("list", "1", "", { options: ["same", "same", "last"], styles: ["multsel"] }),
      control("combo", "2", "", { options: ["same", "same", "last"] }),
      control("list", "3"),
    ]);
    expect(current().selected).toEqual({ "1": [1], "2": [1], "3": [] });
    mutate("1", "check", "2");
    mutate("1", "check-keep", "3");
    expect(current().selected["1"]).toEqual([2, 3]);
    expect(current().values["1"]).toBe("same\nlast");
    mutate("1", "uncheck-keep", "2");
    expect(current().selected["1"]).toEqual([3]);
    mutate("1", "check", "1");
    expect(current().selected["1"]).toEqual([1]);
    mutate("1", "uncheck", "0");
    expect(current().selected["1"]).toEqual([]);
    expect(current().values["1"]).toBe("");
    mutate("2", "check-keep", "2");
    mutate("2", "check-keep", "3");
    expect(current().selected["2"]).toEqual([3]);
    mutate("3", "add", "runtime option");
    expect(current().selected["3"]).toEqual([]);
    expect(current().values["3"]).toBe("");
  });

  it("keeps selected rows attached to their indices through option edits", () => {
    open([control("list", "1", "", { options: ["same", "same", "last"], styles: ["multsel"] })]);
    mutate("1", "check", "2");
    mutate("1", "check-keep", "3");
    mutate("1", "insert", "1 first");
    expect(current().selected["1"]).toEqual([3, 4]);
    expect(current().values["1"]).toBe("same\nlast");
    mutate("1", "replace", "3 changed");
    expect(current().values["1"]).toBe("changed\nlast");
    mutate("1", "delete", "3");
    expect(current().selected["1"]).toEqual([3]);
    expect(current().values["1"]).toBe("last");
    mutate("1", "clear");
    expect(current().options["1"]).toEqual([]);
    expect(current().selected["1"]).toEqual([]);
    expect(current().values["1"]).toBe("");
  });

  it("matches names and controls case-insensitively and retains state on rename and activation", () => {
    open([control("edit", "Edit", "hello"), control("tab", "Second", "Second")], "Settings");
    useDialogs.getState().setValue("SETTINGS", "EDIT", "typed");
    mutate("edit", "focus", "", "settings");
    mutate("edit", "edited-reset", "", "SETTINGS");
    mutate("second", "tab", "", "SETTINGS");
    const before = current();
    mutate("", "rename", "Renamed", "sEtTiNgS");
    expect(current()).toEqual({ ...before, name: "Renamed" });
    expect(current().edited.Edit).toBe(false);
    expect(current().focus).toBe("Second");
    expect(current().activeTab).toBe("Second");
    open([], "other");
    mutate("", "activate", "", "RENAMED");
    expect(useDialogs.getState().dialogs.map((item) => item.name)).toEqual(["other", "Renamed"]);
    expect(useDialogs.getState().dialogs[1].values.Edit).toBe("typed");
    routeDialogEvent({ type: "dialogClose", serverId: "s", name: "rEnAmEd" });
    expect(useDialogs.getState().dialogs.map((item) => item.name)).toEqual(["other"]);
  });
});
