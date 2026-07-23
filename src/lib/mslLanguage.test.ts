import { describe, expect, it } from "vitest";
import { SCRIPT_THEMES, mslDiagnostics, mslTheme } from "./mslLanguage";

describe("mSL editor themes", () => {
  it("offers distinct VS Code, Monokai, and Solarized choices", () => {
    expect(SCRIPT_THEMES.map((theme) => theme.value)).toEqual([
      "vscode-dark",
      "vscode-light",
      "monokai",
      "solarized-dark",
    ]);
    for (const theme of SCRIPT_THEMES) {
      expect(mslTheme(theme.value)).toHaveLength(2);
    }
  });
});

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
