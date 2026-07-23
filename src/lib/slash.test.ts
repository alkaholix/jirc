import { describe, it, expect, beforeEach, vi } from "vitest";

// Slash handling calls into the Tauri command layer; stub it out for tests.
vi.mock("./api", () => ({
  api: {
    sendRaw: vi.fn().mockResolvedValue(undefined),
    scriptRunInput: vi.fn().mockResolvedValue(false),
    scriptRunAlias: vi.fn().mockResolvedValue(false),
    scriptRunCommand: vi.fn().mockResolvedValue(undefined),
  },
}));
vi.mock("./notify", () => ({ notify: vi.fn() }));

import { api } from "./api";
import { handleInput } from "./slash";
import { Buffer } from "../state/store";

const SID = "s1";

/** A channel buffer as handleInput sees it (only serverId/name/kind are read). */
const channel = (name: string) => ({ serverId: SID, name, kind: "channel" }) as Buffer;

beforeEach(() => {
  vi.mocked(api.sendRaw).mockClear();
  vi.mocked(api.scriptRunAlias).mockReset().mockResolvedValue(false);
  vi.mocked(api.scriptRunCommand).mockClear();
});

describe("mIRC alias precedence", () => {
  it("lets a user alias override a frontend built-in command", async () => {
    vi.mocked(api.scriptRunAlias).mockResolvedValueOnce(true);

    await handleInput("/mode +m", channel("#test"));

    expect(api.scriptRunAlias).toHaveBeenCalledWith(SID, "#test", "me", "", "mode", "+m");
    expect(api.sendRaw).not.toHaveBeenCalled();
  });

  it("uses !command to bypass a user alias", async () => {
    vi.mocked(api.scriptRunAlias).mockResolvedValueOnce(true);

    await handleInput("/!mode +m", channel("#test"));

    expect(api.scriptRunAlias).not.toHaveBeenCalled();
    expect(api.sendRaw).toHaveBeenCalledWith(SID, "MODE #test +m");
  });
});

describe("/mode targeting", () => {
  it("passes a nick target through untouched (IRCX +h with a key)", async () => {
    await handleInput("/mode SnueJr +h OWNERKEY", channel("%#Test"));
    expect(api.sendRaw).toHaveBeenCalledWith(SID, "MODE SnueJr +h OWNERKEY");
  });

  it("prepends the active channel to a bare modestring", async () => {
    await handleInput("/mode +h OWNERKEY", channel("%#Test"));
    expect(api.sendRaw).toHaveBeenCalledWith(SID, "MODE %#Test +h OWNERKEY");
  });

  it("does not double an explicit IRCX %# channel target", async () => {
    await handleInput("/mode %#Test +o Bob", channel("%#Test"));
    expect(api.sendRaw).toHaveBeenCalledWith(SID, "MODE %#Test +o Bob");
  });

  it("leaves an explicit # channel target untouched", async () => {
    await handleInput("/mode #other +m", channel("%#Test"));
    expect(api.sendRaw).toHaveBeenCalledWith(SID, "MODE #other +m");
  });
});

describe("// evaluation", () => {
  it("routes //mode $me through the mSL engine so identifiers evaluate", async () => {
    await handleInput("//mode $me +h OWNERKEY", channel("%#Test"));
    expect(api.scriptRunCommand).toHaveBeenCalledWith(SID, "%#Test", "me", "", "mode", "$me +h OWNERKEY");
    expect(api.sendRaw).not.toHaveBeenCalled();
  });

  it("keeps a single-slash /command literal (no engine evaluation)", async () => {
    await handleInput("/mode $me +h OWNERKEY", channel("%#Test"));
    expect(api.sendRaw).toHaveBeenCalledWith(SID, "MODE $me +h OWNERKEY");
    expect(api.scriptRunCommand).not.toHaveBeenCalled();
  });
});
