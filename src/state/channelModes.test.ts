import { describe, expect, it } from "vitest";
import {
  applyModeDelta,
  emptyModes,
  packModeChanges,
  parseChanModeSpec,
} from "./channelModes";

describe("channel mode state", () => {
  const spec = parseChanModeSpec("beI,k,l,imnst");

  it("parses advertised channel mode groups", () => {
    expect(spec).toEqual({
      list: "beI",
      alwaysArg: "k",
      setArg: "l",
      flags: "imnst",
    });
  });

  it("tracks flags and parameter modes using CHANMODES argument rules", () => {
    const initial = applyModeDelta(
      emptyModes(),
      "+imkl",
      ["secret", "25"],
      spec
    ).modes;
    expect([...initial.flags].sort()).toEqual(["i", "m"]);
    expect(initial.values).toEqual({ k: "secret", l: "25" });

    const changed = applyModeDelta(initial, "-k+l", ["secret", "40"], spec).modes;
    expect(changed.values).toEqual({ l: "40" });
  });

  it("returns list changes separately from ordinary modes", () => {
    const result = applyModeDelta(
      emptyModes(),
      "+be-b",
      ["*!*@bad.host", "friend!*@*", "*!*@old.host"],
      spec
    );
    expect(result.listOps).toEqual([
      { mode: "b", adding: true, mask: "*!*@bad.host" },
      { mode: "e", adding: true, mask: "friend!*@*" },
      { mode: "b", adding: false, mask: "*!*@old.host" },
    ]);
  });

  it("consumes nick parameters without treating prefix modes as channel flags", () => {
    const result = applyModeDelta(
      emptyModes(),
      "+oml",
      ["Alice", "30"],
      spec,
      "qaohv"
    );
    expect([...result.modes.flags]).toEqual(["m"]);
    expect(result.modes.values).toEqual({ l: "30" });
  });
});

describe("mode command batching", () => {
  it("preserves signs, arguments, and the server's MODES limit", () => {
    expect(
      packModeChanges(
        "#room",
        [
          { mode: "i", adding: true },
          { mode: "m", adding: true },
          { mode: "k", adding: false, argument: "old-key" },
          { mode: "l", adding: true, argument: "50" },
          { mode: "b", adding: true, argument: "*!*@bad.host" },
        ],
        3
      )
    ).toEqual([
      "MODE #room +im-k old-key",
      "MODE #room +lb 50 *!*@bad.host",
    ]);
  });
});
