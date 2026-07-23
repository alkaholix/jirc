import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { PopupItem } from "../lib/api";
import { PopupItems } from "./popupMenu";

const item = (extra: Partial<PopupItem> = {}): PopupItem => ({
  label: "GateKeeper lookup pending",
  command: "noop",
  separator: false,
  checked: false,
  disabled: false,
  source: "i7.mrc",
  children: [],
  ...extra,
});

describe("PopupItems", () => {
  it("renders $style checked and disabled state", () => {
    const html = renderToStaticMarkup(
      <PopupItems items={[item({ checked: true, disabled: true })]} onRun={vi.fn()} />,
    );
    expect(html).toContain("disabled");
    expect(html).toContain("pmenu-disabled");
    expect(html).toContain("pmenu-check");
    expect(html).toContain("✓");
  });
});
