import { create } from "zustand";
import type { IrcEvent } from "../lib/api";

export interface ScriptToolbarButton {
  name: string;
  tooltip: string;
  icon: string;
  command: string;
  source: string;
  serverId: string;
}

interface ToolbarState {
  buttons: ScriptToolbarButton[];
}

export const useToolbar = create<ToolbarState>(() => ({ buttons: [] }));

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
    };
    if (index < 0) return event.op === "upsert" ? { buttons: [...state.buttons, button] } : state;
    const buttons = [...state.buttons];
    buttons[index] = button;
    return { buttons };
  });
}
