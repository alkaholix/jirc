import { StreamLanguage, StreamParser } from "@codemirror/language";
import { Diagnostic, linter } from "@codemirror/lint";
import { EditorView } from "@codemirror/view";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { tags } from "@lezer/highlight";

interface MslState {
  blockComment: boolean;
}

const keywords = new Set([
  "alias", "on", "menu", "dialog", "if", "elseif", "else", "while",
  "return", "returnex", "halt", "break", "continue", "goto",
]);

const atoms = new Set(["$true", "$false", "$null"]);

const parser: StreamParser<MslState> = {
  startState: () => ({ blockComment: false }),
  token(stream, state) {
    if (state.blockComment) {
      if (stream.skipTo("*/")) {
        stream.match("*/");
        state.blockComment = false;
      } else {
        stream.skipToEnd();
      }
      return "blockComment";
    }
    if (stream.match("/*")) {
      state.blockComment = true;
      return "blockComment";
    }
    if (stream.sol() && stream.match(/\s*;.*/)) return "lineComment";
    if (stream.eatSpace()) return null;
    if (stream.match(/"(?:[^"\\]|\\.)*"?/)) return "string";
    if (stream.match(/\$[!?]?[A-Za-z_][\w.]*(?:\([^)]*\))?/)) {
      return atoms.has(stream.current().toLowerCase()) ? "bool" : "variableName.special";
    }
    if (stream.match(/%[\w.]+/)) return "variableName";
    if (stream.match(/&[\w.]+/)) return "variableName.special";
    if (stream.match(/\/[!.]?[A-Za-z_][\w.-]*/)) return "keyword";
    if (stream.match(/#[A-Za-z_][\w.-]*/)) return "labelName";
    if (stream.match(/\b\d+(?:\.\d+)?\b/)) return "number";
    if (stream.match(/(?:==|!=|<=|>=|&&|\|\||[+\-*\/%=<>])/)) return "operator";
    if (stream.match(/[{}()[\],:|]/)) return "punctuation";
    if (stream.match(/[A-Za-z_][\w.-]*/)) {
      return keywords.has(stream.current().toLowerCase()) ? "keyword" : null;
    }
    stream.next();
    return null;
  },
};

export const mslLanguage = StreamLanguage.define(parser);

export const mslHighlighting = syntaxHighlighting(
  HighlightStyle.define([
    { tag: tags.keyword, color: "#bb9af7", fontWeight: "600" },
    { tag: tags.variableName, color: "#7dcfff" },
    { tag: tags.special(tags.variableName), color: "#2ac3de" },
    { tag: tags.string, color: "#9ece6a" },
    { tag: tags.number, color: "#ff9e64" },
    { tag: tags.bool, color: "#ff9e64", fontWeight: "600" },
    { tag: tags.operator, color: "#89ddff" },
    { tag: tags.labelName, color: "#e0af68" },
    { tag: [tags.lineComment, tags.blockComment], color: "#6f7892", fontStyle: "italic" },
    { tag: tags.punctuation, color: "#a9b1d6" },
  ])
);

export function mslDiagnostics(source: string): Diagnostic[] {
  const diagnostics: Diagnostic[] = [];
  const stack: Array<{ char: string; offset: number }> = [];
  const pairs: Record<string, string> = { "}": "{", ")": "(", "]": "[" };
  let blockComment = false;
  let string = false;
  let lineStart = true;

  for (let i = 0; i < source.length; i += 1) {
    const char = source[i];
    const next = source[i + 1];
    if (blockComment) {
      if (char === "*" && next === "/") {
        blockComment = false;
        i += 1;
      }
      continue;
    }
    if (!string && char === "/" && next === "*") {
      blockComment = true;
      i += 1;
      continue;
    }
    if (!string && lineStart && /\s/.test(char) && char !== "\n") continue;
    if (!string && lineStart && char === ";") {
      const newline = source.indexOf("\n", i);
      if (newline < 0) break;
      i = newline;
      lineStart = true;
      continue;
    }
    if (char === '"' && source[i - 1] !== "\\") string = !string;
    if (!string) {
      if ("{([".includes(char)) stack.push({ char, offset: i });
      else if ("})]".includes(char)) {
        const open = stack.pop();
        if (!open || open.char !== pairs[char]) {
          diagnostics.push({
            from: i,
            to: i + 1,
            severity: "error",
            message: `Unmatched ${char}`,
          });
        }
      }
    }
    lineStart = char === "\n";
  }

  for (const open of stack) {
    diagnostics.push({
      from: open.offset,
      to: open.offset + 1,
      severity: "error",
      message: `Unclosed ${open.char}`,
    });
  }
  if (blockComment) {
    diagnostics.push({
      from: Math.max(0, source.lastIndexOf("/*")),
      to: source.length,
      severity: "error",
      message: "Unclosed block comment",
    });
  }
  if (string) {
    diagnostics.push({
      from: Math.max(0, source.lastIndexOf('"')),
      to: source.length,
      severity: "error",
      message: "Unclosed quoted string",
    });
  }

  let offset = 0;
  for (const line of source.split("\n")) {
    const trimmed = line.trim();
    if (/^alias\s+-/i.test(trimmed) && !/^alias\s+-l(?:\s|$)/i.test(trimmed)) {
      diagnostics.push({
        from: offset + line.indexOf("-"),
        to: offset + line.indexOf("-") + (trimmed.split(/\s+/)[1]?.length ?? 2),
        severity: "warning",
        message: "The supported mIRC alias-definition switch is -l.",
      });
    }
    if (/^alias(?:\s+-l)?\s*(?:\{|$)/i.test(trimmed)) {
      diagnostics.push({
        from: offset,
        to: offset + Math.max(1, line.length),
        severity: "error",
        message: "Alias definition is missing a name.",
      });
    }
    if (/^on\s+/i.test(trimmed) && (trimmed.match(/:/g)?.length ?? 0) < 2) {
      diagnostics.push({
        from: offset,
        to: offset + Math.max(1, line.length),
        severity: "warning",
        message: "This on-event does not have the expected colon-separated fields.",
      });
    }
    offset += line.length + 1;
  }
  return diagnostics;
}

export const mslLinter = linter((view: EditorView) =>
  mslDiagnostics(view.state.doc.toString())
);
