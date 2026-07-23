import { useEffect, useRef } from "react";
import { basicSetup } from "codemirror";
import { EditorState } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import { indentWithTab } from "@codemirror/commands";
import { lintGutter } from "@codemirror/lint";
import { mslHighlighting, mslLanguage, mslLinter } from "../lib/mslLanguage";

export function MslEditor({
  value,
  onChange,
  onSave,
}: {
  value: string;
  onChange: (value: string) => void;
  onSave: () => void;
}) {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const change = useRef(onChange);
  const save = useRef(onSave);
  change.current = onChange;
  save.current = onSave;

  useEffect(() => {
    if (!host.current) return;
    const editor = new EditorView({
      parent: host.current,
      state: EditorState.create({
        doc: value,
        extensions: [
          basicSetup,
          mslLanguage,
          mslHighlighting,
          mslLinter,
          lintGutter(),
          keymap.of([
            indentWithTab,
            {
              key: "Mod-s",
              preventDefault: true,
              run: () => {
                save.current();
                return true;
              },
            },
          ]),
          EditorView.updateListener.of((update) => {
            if (update.docChanged) change.current(update.state.doc.toString());
          }),
          EditorView.theme({
            "&": { height: "100%", backgroundColor: "var(--bg2)", color: "var(--fg)" },
            ".cm-scroller": {
              overflow: "auto",
              fontFamily: '"Cascadia Code", "Consolas", monospace',
              fontSize: "13px",
              lineHeight: "1.5",
            },
            ".cm-gutters": {
              backgroundColor: "var(--panel)",
              color: "var(--muted)",
              borderRight: "1px solid var(--border)",
            },
            ".cm-activeLine, .cm-activeLineGutter": { backgroundColor: "rgba(122,162,247,.08)" },
            ".cm-cursor": { borderLeftColor: "var(--fg)" },
            "&.cm-focused": { outline: "none" },
          }),
        ],
      }),
    });
    view.current = editor;
    return () => {
      editor.destroy();
      view.current = null;
    };
    // The editor owns subsequent document updates.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const editor = view.current;
    if (!editor || editor.state.doc.toString() === value) return;
    editor.dispatch({
      changes: { from: 0, to: editor.state.doc.length, insert: value },
    });
  }, [value]);

  return <div className="msl-editor" ref={host} />;
}
