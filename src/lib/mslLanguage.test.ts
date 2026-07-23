import { describe, expect, it } from "vitest";
import { mslDiagnostics } from "./mslLanguage";

describe("mSL diagnostics", () => {
  it("accepts a normal alias and event", () => {
    expect(
      mslDiagnostics(
        "alias -l helper {\n  if ($1) { echo -a %value }\n}\non *:TEXT:*:#:{ msg # ok }"
      )
    ).toEqual([]);
  });

  it("reports structural errors and malformed definitions", () => {
    const messages = mslDiagnostics(
      "alias -x {\n  echo \"unterminated\n}\non TEXT"
    ).map((item) => item.message);
    expect(messages).toContain("The supported mIRC alias-definition switch is -l.");
    expect(messages).toContain("Unclosed quoted string");
    expect(messages).toContain("This on-event does not have the expected colon-separated fields.");
  });

  it("ignores braces inside comments and strings", () => {
    expect(mslDiagnostics('; }\nalias ok { echo "{" }\n/* } */')).toEqual([]);
  });
});
