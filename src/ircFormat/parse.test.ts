import { describe, it, expect } from "vitest";
import { ReactElement } from "react";
import { parseIrc, stripFormatting } from "./parse";

/** Concatenates the text content of the spans produced by parseIrc. */
function text(nodes: ReturnType<typeof parseIrc>): string {
  return nodes
    .map((n) => (n as ReactElement<{ children: string }>).props.children)
    .join("");
}

describe("stripFormatting", () => {
  it("removes color codes", () => {
    expect(stripFormatting("\x0304red\x03 normal")).toBe("red normal");
    expect(stripFormatting("\x0304,08fg/bg\x03")).toBe("fg/bg");
  });

  it("removes style codes", () => {
    expect(stripFormatting("\x02bold\x02 \x1ditalic\x1d \x1funder\x1f")).toBe(
      "bold italic under"
    );
  });

  it("removes hex colors", () => {
    expect(stripFormatting("\x04ff8800orange")).toBe("orange");
  });
});

describe("parseIrc", () => {
  it("preserves plain text content", () => {
    expect(text(parseIrc("hello world"))).toBe("hello world");
  });

  it("splits on color codes but keeps text", () => {
    const nodes = parseIrc("\x0304red\x0f normal");
    expect(text(nodes)).toBe("red normal");
  });

  it("applies color styling", () => {
    const nodes = parseIrc("\x0304red") as ReactElement<{ style?: { color?: string } }>[];
    const colored = nodes.find((n) => n.props.style?.color);
    expect(colored?.props.style?.color).toBe("#ff0000");
  });

  it("handles fg,bg pairs", () => {
    const nodes = parseIrc("\x0300,01x") as ReactElement<{
      style?: { color?: string; backgroundColor?: string };
    }>[];
    const styled = nodes.find((n) => n.props.style?.backgroundColor);
    expect(styled?.props.style?.color).toBe("#ffffff");
    expect(styled?.props.style?.backgroundColor).toBe("#000000");
  });
});
