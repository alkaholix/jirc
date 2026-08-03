import { create } from "zustand";
import type { IrcEvent } from "../lib/api";

export interface ScriptToolbarButton {
  name: string;
  tooltip: string;
  icon: string;
  command: string;
  source: string;
  serverId: string;
  enabled: boolean;
  visible: boolean;
  checked: boolean;
  separator: boolean;
}

interface ToolbarState {
  buttons: ScriptToolbarButton[];
  visible: boolean;
  setVisible: (visible: boolean) => void;
}

export const useToolbar = create<ToolbarState>((set) => ({
  buttons: [],
  visible: true,
  setVisible: (visible) => set({ visible }),
}));

export function routeToolbarEvent(event: IrcEvent): void {
  if (event.type !== "toolbar") return;
  useToolbar.setState((state) => {
    if (event.op === "clear") return { buttons: [] };
    const index = state.buttons.findIndex(
      (button) => button.name.toLowerCase() === event.name.toLowerCase()
    );
    if (event.op === "delete") {
      return index < 0
        ? state
        : { buttons: state.buttons.filter((_, buttonIndex) => buttonIndex !== index) };
    }
    const existing = index >= 0 ? state.buttons[index] : undefined;
    const button: ScriptToolbarButton = {
      name: event.name,
      tooltip: event.op === "tooltip" ? event.tooltip : existing?.tooltip ?? event.tooltip,
      icon: event.op === "icon" ? event.icon : existing?.icon ?? event.icon,
      command: event.op === "command" ? event.command : existing?.command ?? event.command,
      source: event.source || existing?.source || "",
      serverId: event.serverId || existing?.serverId || "",
      enabled: event.op === "enabled" ? event.command !== "0" : existing?.enabled ?? true,
      visible: event.op === "visible" ? event.command !== "0" : existing?.visible ?? true,
      checked: event.op === "checked" ? event.command !== "0" : existing?.checked ?? false,
      separator: event.op === "separator" || existing?.separator || false,
    };
    if (index < 0) return ["upsert", "separator"].includes(event.op) ? { buttons: [...state.buttons, button] } : state;
    const buttons = [...state.buttons];
    buttons[index] = button;
    return { buttons };
  });
}
