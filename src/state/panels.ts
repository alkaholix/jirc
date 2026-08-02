import { create } from "zustand";
import type { IrcEvent } from "../lib/api";

export interface ScriptPanelItem {
  id: string;
  kind: "text" | "button" | "input" | "checkbox" | "progress" | "separator";
  label: string;
  value: string;
  command: string;
  source: string;
}

export interface ScriptPanel {
  name: string;
  title: string;
  serverId: string;
  items: ScriptPanelItem[];
}

interface PanelState {
  panels: ScriptPanel[];
}

export const usePanels = create<PanelState>(() => ({ panels: [] }));

export function routePanelEvent(event: IrcEvent): void {
  if (event.type !== "panel") return;
  usePanels.setState((state) => {
    if (event.op === "clear") return { panels: [] };
    const panelIndex = state.panels.findIndex(
      (panel) => panel.name.toLowerCase() === event.panel.toLowerCase()
    );
    if (event.op === "deletePanel") {
      return panelIndex < 0
        ? state
        : { panels: state.panels.filter((_, index) => index !== panelIndex) };
    }
    if (event.op === "upsert") {
      const next: ScriptPanel = {
        name: event.panel,
        title: event.label || event.panel,
        serverId: event.serverId,
        items: panelIndex >= 0 ? state.panels[panelIndex].items : [],
      };
      if (panelIndex < 0) return { panels: [...state.panels, next] };
      const panels = [...state.panels];
      panels[panelIndex] = next;
      return { panels };
    }
    if (panelIndex < 0) return state;
    const panel = state.panels[panelIndex];
    const itemIndex = panel.items.findIndex(
      (item) => item.id.toLowerCase() === event.id.toLowerCase()
    );
    if (event.op === "deleteItem") {
      if (itemIndex < 0) return state;
      const panels = [...state.panels];
      panels[panelIndex] = {
        ...panel,
        items: panel.items.filter((_, index) => index !== itemIndex),
      };
      return { panels };
    }
    if (!["text", "button", "input", "checkbox", "progress", "separator"].includes(event.op)) return state;
    const item: ScriptPanelItem = {
      id: event.id,
      kind: event.op as ScriptPanelItem["kind"],
      label: event.label,
      value: event.value,
      command: event.command,
      source: event.source,
    };
    const items = [...panel.items];
    if (itemIndex < 0) items.push(item);
    else items[itemIndex] = item;
    const panels = [...state.panels];
    panels[panelIndex] = { ...panel, serverId: event.serverId, items };
    return { panels };
  });
}
