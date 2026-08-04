import { useState, type MouseEvent } from "react";
import { api, type PopupItem } from "../lib/api";
import type { Buffer, Server } from "../state/store";
import { useSettings } from "../state/settings";
import { showNativePopup } from "../lib/nativePopup";
import { ContextMenu, PopupItems } from "./popupMenu";

export function ScriptMenubarMenu({ buffer, server }: { buffer: Buffer | null; server?: Server }) {
  const native = useSettings((state) => state.nativePopupMenus);
  const [menu, setMenu] = useState<{ x: number; y: number; items: PopupItem[] } | null>(null);
  if (!buffer) return null;

  const target = buffer.kind === "status" ? "" : buffer.name;
  const run = (item: PopupItem) => {
    void api.scriptRunPopup(
      buffer.serverId, target, server?.nick ?? "", server?.name ?? "",
      item.command, [], undefined, item.source, "menubar"
    );
    setMenu(null);
  };
  const open = async (event: MouseEvent<HTMLButtonElement>) => {
    const rect = event.currentTarget.getBoundingClientRect();
    const items = await api.scriptPopups(
      buffer.serverId, target, server?.nick ?? "", server?.name ?? "", "menubar", ""
    ).catch(() => []);
    if (!items.length) return;
    if (native && await showNativePopup(items, rect.left, rect.bottom, run)) return;
    setMenu({ x: rect.left, y: rect.bottom, items });
  };

  return <>
    <button onClick={open}>Commands</button>
    {menu && <ContextMenu x={menu.x} y={menu.y} onClose={() => setMenu(null)}>
      <PopupItems items={menu.items} onRun={run} />
    </ContextMenu>}
  </>;
}
