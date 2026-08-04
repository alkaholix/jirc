import { beforeEach, describe, expect, it, vi } from "vitest";

const { created, popup, close } = vi.hoisted(() => ({
  created: [] as Array<{ kind: string; options: Record<string, unknown> }>,
  popup: vi.fn().mockResolvedValue(undefined),
  close: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("@tauri-apps/api/menu", () => ({
  Menu: { new: vi.fn(async (options: Record<string, unknown>) => ({ options, popup, close })) },
  MenuItem: { new: vi.fn(async (options: Record<string, unknown>) => {
    const item = { kind: "item", options }; created.push(item); return item;
  }) },
  CheckMenuItem: { new: vi.fn(async (options: Record<string, unknown>) => {
    const item = { kind: "check", options }; created.push(item); return item;
  }) },
  PredefinedMenuItem: { new: vi.fn(async (options: Record<string, unknown>) => {
    const item = { kind: "separator", options }; created.push(item); return item;
  }) },
  Submenu: { new: vi.fn(async (options: Record<string, unknown>) => {
    const item = { kind: "submenu", options }; created.push(item); return item;
  }) },
}));
vi.mock("@tauri-apps/api/dpi", () => ({
  LogicalPosition: class { constructor(public x: number, public y: number) {} },
}));

import { showNativePopup } from "./nativePopup";

beforeEach(() => {
  created.length = 0;
  popup.mockClear();
  close.mockClear();
  Object.defineProperty(window, "__TAURI_INTERNALS__", { value: {}, configurable: true });
});

describe("native script popups", () => {
  it("preserves checked, disabled, separators and nested items", async () => {
    const run = vi.fn();
    const shown = await showNativePopup([
      { label: "Checked", command: "one", separator: false, checked: true, children: [] },
      { label: "Disabled", command: "two", separator: false, disabled: true, children: [] },
      { label: "", command: "", separator: true, children: [] },
      { label: "More", command: "", separator: false, children: [
        { label: "Child", command: "child", separator: false, children: [] },
      ] },
    ], 10, 20, run);

    expect(shown).toBe(true);
    expect(created.map((item) => item.kind)).toEqual(["check", "item", "separator", "item", "submenu"]);
    expect(created[1].options.enabled).toBe(false);
    expect(popup).toHaveBeenCalled();
    expect(close).toHaveBeenCalled();
  });
});
