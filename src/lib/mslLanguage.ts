import { StreamLanguage, StreamParser } from "@codemirror/language";
import { Diagnostic, linter } from "@codemirror/lint";
import { EditorView } from "@codemirror/view";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { tags } from "@lezer/highlight";
import type { Extension } from "@codemirror/state";
import type { ScriptTheme } from "../state/settings";

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

interface EditorPalette {
  background: string;
  foreground: string;
  gutter: string;
  gutterText: string;
  activeLine: string;
  selection: string;
  keyword: string;
  variable: string;
  special: string;
  string: string;
  number: string;
  operator: string;
  label: string;
  comment: string;
  punctuation: string;
  dark: boolean;
}

export const SCRIPT_THEMES: ReadonlyArray<{ value: ScriptTheme; label: string }> = [
  { value: "vscode-dark", label: "VS Code Dark+" },
  { value: "vscode-light", label: "VS Code Light+" },
  { value: "monokai", label: "Monokai" },
  { value: "solarized-dark", label: "Solarized Dark" },
];

const palettes: Record<ScriptTheme, EditorPalette> = {
  "vscode-dark": {
    background: "#1e1e1e", foreground: "#d4d4d4", gutter: "#1e1e1e",
    gutterText: "#858585", activeLine: "#2a2d2e", selection: "#264f78",
    keyword: "#c586c0", variable: "#9cdcfe", special: "#4fc1ff",
    string: "#ce9178", number: "#b5cea8", operator: "#d4d4d4",
    label: "#dcdcaa", comment: "#6a9955", punctuation: "#d4d4d4", dark: true,
  },
  "vscode-light": {
    background: "#ffffff", foreground: "#000000", gutter: "#ffffff",
    gutterText: "#237893", activeLine: "#f3f3f3", selection: "#add6ff",
    keyword: "#af00db", variable: "#001080", special: "#0070c1",
    string: "#a31515", number: "#098658", operator: "#000000",
    label: "#795e26", comment: "#008000", punctuation: "#000000", dark: false,
  },
  monokai: {
    background: "#272822", foreground: "#f8f8f2", gutter: "#272822",
    gutterText: "#90908a", activeLine: "#3e3d32", selection: "#49483e",
    keyword: "#f92672", variable: "#fd971f", special: "#66d9ef",
    string: "#e6db74", number: "#ae81ff", operator: "#f92672",
    label: "#a6e22e", comment: "#75715e", punctuation: "#f8f8f2", dark: true,
  },
  "solarized-dark": {
    background: "#002b36", foreground: "#839496", gutter: "#073642",
    gutterText: "#586e75", activeLine: "#073642", selection: "#274b52",
    keyword: "#859900", variable: "#268bd2", special: "#2aa198",
    string: "#2aa198", number: "#d33682", operator: "#859900",
    label: "#b58900", comment: "#586e75", punctuation: "#839496", dark: true,
  },
};

export function mslTheme(theme: ScriptTheme): Extension {
  const palette = palettes[theme] ?? palettes["vscode-dark"];
  return [
    EditorView.theme(
      {
        "&": { height: "100%", backgroundColor: palette.background, color: palette.foreground },
        ".cm-scroller": {
          overflow: "auto",
          fontFamily: '"Cascadia Code", "Consolas", monospace',
          fontSize: "13px",
          lineHeight: "1.5",
        },
        ".cm-content, .cm-gutter": { caretColor: palette.foreground },
        ".cm-gutters": {
          backgroundColor: palette.gutter,
          color: palette.gutterText,
          borderRight: `1px solid ${palette.activeLine}`,
        },
        ".cm-activeLine, .cm-activeLineGutter": { backgroundColor: palette.activeLine },
        ".cm-selectionBackground, ::selection": { backgroundColor: `${palette.selection} !important` },
        ".cm-cursor": { borderLeftColor: palette.foreground },
        "&.cm-focused": { outline: "none" },
      },
      { dark: palette.dark }
    ),
    syntaxHighlighting(
      HighlightStyle.define([
        { tag: tags.keyword, color: palette.keyword, fontWeight: "600" },
        { tag: tags.variableName, color: palette.variable },
        { tag: tags.special(tags.variableName), color: palette.special },
        { tag: tags.string, color: palette.string },
        { tag: tags.number, color: palette.number },
        { tag: tags.bool, color: palette.number, fontWeight: "600" },
        { tag: tags.operator, color: palette.operator },
        { tag: tags.labelName, color: palette.label },
        { tag: [tags.lineComment, tags.blockComment], color: palette.comment, fontStyle: "italic" },
        { tag: tags.punctuation, color: palette.punctuation },
      ])
    ),
  ];
}

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
