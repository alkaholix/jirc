import { describe, expect, it } from "vitest";
import { extractPreviewUrls } from "./UrlPreviews";

describe("URL preview extraction", () => {
  it("extracts unique HTTP links, trims sentence punctuation, and limits message fan-out", () => {
    expect(extractPreviewUrls(
      "See https://one.test/a, https://two.test/b! https://one.test/a and https://three.test/c https://four.test/d"
    )).toEqual([
      "https://one.test/a",
      "https://two.test/b",
      "https://three.test/c",
    ]);
  });

  it("does not treat non-HTTP schemes as previewable", () => {
    expect(extractPreviewUrls("irc://irc.test/#room ftp://files.test/a")).toEqual([]);
  });
});
