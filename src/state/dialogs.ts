import { create } from "zustand";
import { DialogControl, IrcEvent } from "../lib/api";

/// A currently-open script dialog and its live control values.
export interface OpenDialog {
  serverId: string;
  name: string;
  title: string;
  controls: DialogControl[];
  /** control id -> current value (edit text, "1"/"0" for checks, selection for combo/list). */
  values: Record<string, string>;
  /** control id -> options, including any added at runtime via /did -a. */
  options: Record<string, string[]>;
}

interface DialogState {
  dialogs: OpenDialog[];
  setValue: (name: string, control: string, value: string) => void;
  close: (name: string) => void;
}

export const useDialogs = create<DialogState>((set) => ({
  dialogs: [],
  setValue: (name, control, value) =>
    set((s) => ({
      dialogs: s.dialogs.map((d) =>
        d.name === name ? { ...d, values: { ...d.values, [control]: value } } : d
      ),
    })),
  close: (name) => set((s) => ({ dialogs: s.dialogs.filter((d) => d.name !== name) })),
}));

/** Default value for a control when the dialog opens. */
function initialValues(controls: DialogControl[]): Record<string, string> {
  const v: Record<string, string> = {};
  for (const c of controls) {
    if (c.kind === "edit" || c.kind === "editbox") v[c.id] = c.label;
    else if (c.kind === "check") v[c.id] = "0";
    else if (c.kind === "combo" || c.kind === "list") v[c.id] = c.options[0] ?? "";
  }
  return v;
}

/** Routes dialog-related backend events into the dialog store. */
export function routeDialogEvent(ev: IrcEvent) {
  if (ev.type === "dialogOpen") {
    const options: Record<string, string[]> = {};
    for (const c of ev.controls) if (c.options.length) options[c.id] = [...c.options];
    useDialogs.setState((s) => ({
      dialogs: [
        ...s.dialogs.filter((d) => d.name !== ev.name),
        {
          serverId: ev.serverId,
          name: ev.name,
          title: ev.title,
          controls: ev.controls,
          values: initialValues(ev.controls),
          options,
        },
      ],
    }));
  } else if (ev.type === "dialogClose") {
    useDialogs.getState().close(ev.name);
  } else if (ev.type === "dialogSet") {
    useDialogs.setState((s) => ({
      dialogs: s.dialogs.map((d) => {
        if (d.name !== ev.dialog) return d;
        if (ev.op === "add") {
          return {
            ...d,
            options: { ...d.options, [ev.control]: [...(d.options[ev.control] ?? []), ev.value] },
          };
        }
        if (ev.op === "clear") {
          return {
            ...d,
            options: { ...d.options, [ev.control]: [] },
            values: { ...d.values, [ev.control]: "" },
          };
        }
        return { ...d, values: { ...d.values, [ev.control]: ev.value } };
      }),
    }));
  }
}
