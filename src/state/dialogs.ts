import { create } from "zustand";
import { DialogControl, IrcEvent } from "../lib/api";

/// A currently-open script dialog and its live control values.
export interface OpenDialog {
  instanceId: number;
  serverId: string;
  name: string;
  title: string;
  controls: DialogControl[];
  /** control id -> current value (edit text, "1"/"0" for checks, selection for combo/list). */
  values: Record<string, string>;
  /** control id -> options, including any added at runtime via /did -a. */
  options: Record<string, string[]>;
  /** control id -> one-based selected option indices (text need not be unique). */
  selected: Record<string, number[]>;
  width: number;
  height: number;
  activeTab: string;
  edited: Record<string, boolean>;
  focus: string;
}

interface DialogState {
  dialogs: OpenDialog[];
  setValue: (name: string, control: string, value: string, selected?: number[]) => void;
  close: (name: string) => void;
}

const sameId = (left: string, right: string) => left.toLowerCase() === right.toLowerCase();
const isOptionsControl = (control: DialogControl) => control.kind === "list" || control.kind === "combo";
const isCheckControl = (control: DialogControl) => control.kind === "check" || control.kind === "radio";
const controlId = (dialog: OpenDialog, id: string) =>
  dialog.controls.find((control) => sameId(control.id, id))?.id ?? id.toLowerCase();

function optionSelection(options: string[], selected: number[]): number[] {
  return [...new Set(selected.filter((index) => Number.isInteger(index) && index > 0 && index <= options.length))]
    .sort((left, right) => left - right);
}

const selectionText = (options: string[], selected: number[]) =>
  selected.map((index) => options[index - 1]).join("\n");

let nextDialogInstance = 0;

export const useDialogs = create<DialogState>((set) => ({
  dialogs: [],
  setValue: (name, control, value, selected) =>
    set((s) => ({
      dialogs: s.dialogs.map((d) => {
        if (!sameId(d.name, name)) return d;
        const id = controlId(d, control);
        return {
          ...d,
          values: { ...d.values, [id]: value },
          selected: selected === undefined ? d.selected : {
            ...d.selected,
            [id]: optionSelection(d.options[id] ?? [], selected),
          },
          edited: { ...d.edited, [id]: true },
        };
      }),
    })),
  close: (name) => set((s) => ({ dialogs: s.dialogs.filter((d) => !sameId(d.name, name)) })),
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
    else v[c.id] = c.label;
  }
  return v;
}

function setText(dialog: OpenDialog, control: DialogControl, text: string): OpenDialog {
  const id = control.id;
  return {
    ...dialog,
    controls: dialog.controls.map((item) => item.id === id ? { ...item, label: text } : item),
    values: isCheckControl(control) ? dialog.values : { ...dialog.values, [id]: text },
    selected: isOptionsControl(control) ? { ...dialog.selected, [id]: [] } : dialog.selected,
  };
}

function setOptions(dialog: OpenDialog, id: string, options: string[], selected: number[]): OpenDialog {
  const indices = optionSelection(options, selected);
  return {
    ...dialog,
    options: { ...dialog.options, [id]: options },
    selected: { ...dialog.selected, [id]: indices },
    values: {
      ...dialog.values,
      [id]: indices.length || dialog.selected[id]?.length
        ? selectionText(options, indices)
        : dialog.values[id] ?? "",
    },
  };
}

/** Routes dialog-related backend events into the dialog store. */
export function routeDialogEvent(ev: IrcEvent) {
  if (ev.type === "dialogOpen") {
    const options: Record<string, string[]> = {};
    const selected: Record<string, number[]> = {};
    for (const c of ev.controls) {
      if (isOptionsControl(c)) {
        options[c.id] = [...c.options];
        selected[c.id] = c.options.length ? [1] : [];
      }
    }
    useDialogs.setState((s) => ({
      dialogs: [
        ...s.dialogs.filter((d) => !sameId(d.name, ev.name)),
        {
          instanceId: ++nextDialogInstance,
          serverId: ev.serverId,
          name: ev.name,
          title: ev.title,
          controls: ev.controls,
          values: initialValues(ev.controls),
          options,
          selected,
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
    if (ev.op === "activate") {
      useDialogs.setState((s) => {
        const dialog = s.dialogs.find((item) => sameId(item.name, ev.dialog));
        return dialog ? { dialogs: [...s.dialogs.filter((item) => item !== dialog), dialog] } : s;
      });
      return;
    }
    useDialogs.setState((s) => ({
      dialogs: s.dialogs.map((d) => {
        if (!sameId(d.name, ev.dialog)) return d;
        if (ev.op === "title") return { ...d, title: ev.value };
        if (ev.op === "rename") return { ...d, name: ev.value || d.name };
        if (ev.op === "size") {
          const [, , width, height] = ev.value.split(/\s+/).map(Number);
          return { ...d, width: width || d.width, height: height || d.height };
        }
        const id = controlId(d, ev.control);
        const target = d.controls.find((control) => control.id === id);
        if (!target) return d;
        if (ev.op === "focus") return { ...d, focus: id };
        if (ev.op === "tab") return { ...d, activeTab: id, focus: id };
        if (ev.op === "edited-reset") return { ...d, edited: { ...d.edited, [id]: false } };
        if (ev.op === "range") {
          return {
            ...d,
            controls: d.controls.map((control) =>
              control.id === id
                ? { ...control, styles: [...control.styles.filter((style) => style !== "range"), "range", ...ev.value.split(/\s+/)] }
                : control
            ),
          };
        }
        if (["enable", "disable", "show", "hide", "default"].includes(ev.op)) {
          return {
            ...d,
            controls: d.controls.map((control) =>
              control.id === id
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
        if (["check", "uncheck", "check-keep", "uncheck-keep", "indeterminate"].includes(ev.op)) {
          if (isOptionsControl(target)) {
            const options = d.options[id] ?? [];
            const position = Number(ev.value.trim());
            const valid = Number.isInteger(position) && position > 0 && position <= options.length;
            let selected = d.selected[id] ?? [];
            if (ev.op === "check" || ev.op === "check-keep") {
              selected = valid
                ? ev.op === "check-keep" && target.kind === "list" ? [...selected, position] : [position]
                : ev.op === "check" ? [] : selected;
            } else if (ev.op === "uncheck" || ev.op === "uncheck-keep") {
              selected = position > 0 ? selected.filter((index) => index !== position)
                : ev.op === "uncheck" ? [] : selected;
            } else return d;
            return {
              ...d,
              selected: { ...d.selected, [id]: optionSelection(options, selected) },
              values: { ...d.values, [id]: selectionText(options, optionSelection(options, selected)) },
            };
          }
          if (!isCheckControl(target)) return d;
          return {
            ...d,
            values: {
              ...d.values,
              [id]: ev.op.startsWith("check") ? "1" : ev.op === "indeterminate" ? "2" : "0",
            },
          };
        }
        if (ev.op === "add") {
          if (!isOptionsControl(target)) {
            const text = target.kind === "edit" || target.kind === "editbox"
              ? d.values[id] ?? "" : target.label;
            return setText(d, target, text + ev.value);
          }
          return {
            ...d,
            options: { ...d.options, [id]: [...(d.options[id] ?? []), ev.value] },
          };
        }
        if (ev.op === "insert" || ev.op === "replace" || ev.op === "delete") {
          if (!isOptionsControl(target)) return d;
          const [positionText, ...textParts] = ev.value.split(" ");
          const position = Math.max(1, Number(positionText) || 1) - 1;
          const options = [...(d.options[id] ?? [])];
          let selected = d.selected[id] ?? [];
          if (ev.op === "insert") {
            const insertion = Math.min(position, options.length);
            options.splice(insertion, 0, textParts.join(" "));
            selected = selected.map((index) => index > insertion ? index + 1 : index);
          }
          else if (ev.op === "replace" && position < options.length) options[position] = textParts.join(" ");
          else if (ev.op === "delete" && position < options.length) {
            options.splice(position, 1);
            selected = selected.filter((index) => index !== position + 1)
              .map((index) => index > position + 1 ? index - 1 : index);
          }
          return setOptions(d, id, options, selected);
        }
        if (ev.op === "clear") {
          const cleared = setText(d, target, "");
          return {
            ...cleared,
            options: { ...cleared.options, [id]: [] },
            selected: { ...cleared.selected, [id]: [] },
          };
        }
        return setText(d, target, ev.value);
      }),
    }));
  }
}
