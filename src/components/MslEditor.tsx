import { useEffect, useRef } from "react";
import { basicSetup } from "codemirror";
import { Compartment, EditorState } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import { indentWithTab } from "@codemirror/commands";
import { lintGutter } from "@codemirror/lint";
import { mslLanguage, mslLinter, mslTheme } from "../lib/mslLanguage";
import type { ScriptTheme } from "../state/settings";

export function MslEditor({
  value,
  onChange,
  onSave,
  theme,
}: {
  value: string;
  onChange: (value: string) => void;
  onSave: () => void;
  theme: ScriptTheme;
}) {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const change = useRef(onChange);
  const save = useRef(onSave);
  const themeCompartment = useRef(new Compartment());
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
          themeCompartment.current.of(mslTheme(theme)),
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

  useEffect(() => {
    view.current?.dispatch({
      effects: themeCompartment.current.reconfigure(mslTheme(theme)),
    });
  }, [theme]);

  return <div className="msl-editor" ref={host} />;
}
