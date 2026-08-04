import { CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu } from "@tauri-apps/api/menu";
import { LogicalPosition } from "@tauri-apps/api/dpi";
import type { PopupItem } from "./api";

type NativeItem = MenuItem | CheckMenuItem | PredefinedMenuItem | Submenu;

async function convert(items: PopupItem[], onRun: (item: PopupItem) => void): Promise<NativeItem[]> {
  const result: NativeItem[] = [];
  for (const item of items) {
    if (item.separator) {
      result.push(await PredefinedMenuItem.new({ item: "Separator" }));
    } else if (item.children.length > 0) {
      result.push(await Submenu.new({
        text: item.label,
        enabled: !item.disabled,
        items: await convert(item.children, onRun),
      }));
    } else if (item.checked) {
      result.push(await CheckMenuItem.new({
        text: item.label,
        checked: true,
        enabled: !item.disabled,
        action: () => onRun(item),
      }));
    } else {
      result.push(await MenuItem.new({
        text: item.label,
        enabled: !item.disabled,
        action: () => onRun(item),
      }));
    }
  }
  return result;
}

/** Shows evaluated script popup items using the operating system menu. Returns
 * false when unavailable so callers can retain the WebView menu fallback. */
export async function showNativePopup(
  items: PopupItem[],
  x: number,
  y: number,
  onRun: (item: PopupItem) => void
): Promise<boolean> {
  if (!("__TAURI_INTERNALS__" in window) || items.length === 0) return false;
  try {
    const menu = await Menu.new({ items: await convert(items, onRun) });
    await menu.popup(new LogicalPosition(x, y));
    await menu.close();
    return true;
  } catch {
    return false;
  }
}
