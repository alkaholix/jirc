import { beforeEach, describe, expect, it } from "vitest";
import { runIgnore, runNotify, runUnignore } from "./clientLists";
import { useSettings } from "../state/settings";

const ignores = () => useSettings.getState().ignores;
const notifies = () => useSettings.getState().notifyList;

beforeEach(() => {
  useSettings.getState().set("ignores", []);
  useSettings.getState().set("notifyList", []);
});

describe("/ignore switches", () => {
  it("never stores a switch as the nick", () => {
    // The bug this guards: `-pc` was read as the name to ignore, so the list
    // filled with switch tokens and the actual nick was dropped.
    runIgnore("-pc bob");
    expect(ignores()).toEqual(["bob"]);
  });

  it("says so when asked for a narrower ignore than it can do", () => {
    const lines = runIgnore("-c bob");
    expect(lines.some((l) => l.includes("all-or-nothing"))).toBe(true);
    // -a *is* jIRC's behaviour, so it passes without comment.
    expect(runIgnore("-a sam").some((l) => l.includes("all-or-nothing"))).toBe(false);
  });

  it("removes one entry with -r and all of them with a bare -r", () => {
    runIgnore("bob");
    runIgnore("sam");
    runIgnore("-r bob");
    expect(ignores()).toEqual(["sam"]);
    runIgnore("-r");
    expect(ignores()).toEqual([]);
  });

  it("lists with -l and with no arguments, without adding anything", () => {
    runIgnore("bob");
    expect(runIgnore("-l")[0]).toContain("bob");
    expect(runIgnore("")[0]).toContain("bob");
    expect(ignores()).toEqual(["bob"]);
  });

  it("still supports /unignore", () => {
    runIgnore("bob");
    runUnignore("bob");
    expect(ignores()).toEqual([]);
  });
});

describe("/notify switches", () => {
  it("never stores a switch as the nick", () => {
    runNotify("-l");
    expect(notifies()).toEqual([]);
    runNotify("bob");
    expect(notifies()).toEqual(["bob"]);
  });

  it("consumes the network token after -n so the nick still parses", () => {
    runNotify("-n Libera bob");
    expect(notifies()).toEqual(["bob"]);
  });

  it("consumes the window switches jIRC has no equivalent of", () => {
    // -s/-h show and hide mIRC's notify window; jIRC keeps the list in
    // Settings, so they must not be mistaken for a nickname.
    runNotify("-s");
    runNotify("-h");
    expect(notifies()).toEqual([]);
  });

  it("removes with -r", () => {
    runNotify("bob");
    runNotify("-r bob");
    expect(notifies()).toEqual([]);
  });
});
