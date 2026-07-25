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
  width: number;
  height: number;
  activeTab: string;
  edited: Record<string, boolean>;
  focus: string;
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
        d.name === name
          ? {
              ...d,
              values: { ...d.values, [control]: value },
              edited: { ...d.edited, [control]: true },
            }
          : d
      ),
    })),
  close: (name) => set((s) => ({ dialogs: s.dialogs.filter((d) => d.name !== name) })),
}));

/** Default value for a control when the dialog opens. */
function initialValues(controls: DialogControl[]): Record<string, string> {
  const v: Record<string, string> = {};
  for (const c of controls) {
    if (c.kind === "edit" || c.kind === "editbox") v[c.id] = c.label;
    else if (c.kind === "check" || c.kind === "radio") v[c.id] = "0";
    else if (c.kind === "scroll") {
      const position = c.styles.findIndex((style) => style === "pos");
      v[c.id] = position >= 0 ? c.styles[position + 1] ?? "0" : "0";
    }
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
          width: ev.width,
          height: ev.height,
          activeTab: ev.controls.find((control) => control.kind === "tab")?.id ?? "",
          edited: {},
          focus: "",
        },
      ],
    }));
  } else if (ev.type === "dialogClose") {
    useDialogs.getState().close(ev.name);
  } else if (ev.type === "dialogSet") {
    useDialogs.setState((s) => ({
      dialogs: s.dialogs.map((d) => {
        if (d.name !== ev.dialog) return d;
        if (ev.op === "title") return { ...d, title: ev.value };
        if (ev.op === "rename") return { ...d, name: ev.value || d.name };
        if (ev.op === "size") {
          const [, , width, height] = ev.value.split(/\s+/).map(Number);
          return { ...d, width: width || d.width, height: height || d.height };
        }
        if (ev.op === "focus") return { ...d, focus: ev.control };
        if (ev.op === "range") {
          return {
            ...d,
            controls: d.controls.map((control) =>
              control.id === ev.control
                ? { ...control, styles: [...control.styles.filter((style) => style !== "range"), "range", ...ev.value.split(/\s+/)] }
                : control
            ),
          };
        }
        if (["enable", "disable", "show", "hide", "default"].includes(ev.op)) {
          return {
            ...d,
            controls: d.controls.map((control) =>
              control.id === ev.control
                ? {
                    ...control,
                    enabled: ev.op === "enable" ? true : ev.op === "disable" ? false : control.enabled,
                    visible: ev.op === "show" ? true : ev.op === "hide" ? false : control.visible,
                    default: ev.op === "default" ? true : control.default,
                  }
                : ev.op === "default"
                  ? { ...control, default: false }
                  : control
            ),
          };
        }
        if (ev.op === "check" || ev.op === "uncheck" || ev.op === "indeterminate") {
          return {
            ...d,
            values: {
              ...d.values,
              [ev.control]: ev.op === "check" ? "1" : ev.op === "indeterminate" ? "2" : "0",
            },
          };
        }
        if (ev.op === "add") {
          return {
            ...d,
            options: { ...d.options, [ev.control]: [...(d.options[ev.control] ?? []), ev.value] },
          };
        }
        if (ev.op === "insert" || ev.op === "replace" || ev.op === "delete") {
          const [positionText, ...textParts] = ev.value.split(" ");
          const position = Math.max(1, Number(positionText) || 1) - 1;
          const options = [...(d.options[ev.control] ?? [])];
          if (ev.op === "insert") options.splice(position, 0, textParts.join(" "));
          else if (ev.op === "replace" && position < options.length) options[position] = textParts.join(" ");
          else if (ev.op === "delete" && position < options.length) options.splice(position, 1);
          return { ...d, options: { ...d.options, [ev.control]: options } };
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
