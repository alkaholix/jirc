import { useEffect, useState, type MouseEvent } from "react";
import { api, type PopupItem } from "../lib/api";
import type { Buffer, Server } from "../state/store";
import { useSettings } from "../state/settings";
import { showNativePopup } from "../lib/nativePopup";
import { ContextMenu, PopupItems } from "./popupMenu";

/// Script-defined menubar menus (`menu menubar { … }`).
///
/// The top bar is otherwise icons only, so this renders **nothing at all**
/// until a script actually defines menubar items — rather than sitting there as
/// a button that does nothing when clicked. The check costs one lookup per
/// buffer switch; the items are re-fetched on open so dynamic `$iif` labels and
/// `$style` states still evaluate fresh.
export function ScriptMenubarMenu({ buffer, server }: { buffer: Buffer | null; server?: Server }) {
  const native = useSettings((state) => state.nativePopupMenus);
  const [menu, setMenu] = useState<{ x: number; y: number; items: PopupItem[] } | null>(null);
  const [available, setAvailable] = useState(false);

  const serverId = buffer?.serverId;
  const target = !buffer || buffer.kind === "status" ? "" : buffer.name;
  const nick = server?.nick ?? "";
  const network = server?.name ?? "";

  useEffect(() => {
    if (!serverId) {
      setAvailable(false);
      return;
    }
    let cancelled = false;
    api
      .scriptPopups(serverId, target, nick, network, "menubar", "")
      .then((items) => {
        if (!cancelled) setAvailable(items.length > 0);
      })
      .catch(() => {
        if (!cancelled) setAvailable(false);
      });
    return () => {
      cancelled = true;
    };
  }, [serverId, target, nick, network]);

  if (!buffer || !available) return null;

  const run = (item: PopupItem) => {
    void api.scriptRunPopup(
      buffer.serverId, target, nick, network,
      item.command, [], undefined, item.source, "menubar"
    );
    setMenu(null);
  };
  const open = async (event: MouseEvent<HTMLButtonElement>) => {
    const rect = event.currentTarget.getBoundingClientRect();
    const items = await api.scriptPopups(
      buffer.serverId, target, nick, network, "menubar", ""
    ).catch(() => []);
    if (!items.length) {
      setAvailable(false);
      return;
    }
    if (native && await showNativePopup(items, rect.left, rect.bottom, run)) return;
    setMenu({ x: rect.left, y: rect.bottom, items });
  };

  return <>
    <button className="icon-btn" onClick={open} title="Script menu">
      ≡
    </button>
    {menu && <ContextMenu x={menu.x} y={menu.y} onClose={() => setMenu(null)}>
      <PopupItems items={menu.items} onRun={run} />
    </ContextMenu>}
  </>;
}
