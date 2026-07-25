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
});
