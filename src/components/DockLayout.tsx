import { type CSSProperties, type DragEvent, type PointerEvent, type ReactNode, useState } from "react";
import { type DockPaneId, type DockSide, useSettings } from "../state/settings";

export interface DockPane {
  id: DockPaneId;
  label: string;
  content: ReactNode;
}

export function moveDockPane(
  order: DockPaneId[],
  pane: DockPaneId,
  before?: DockPaneId,
): DockPaneId[] {
  const next = order.filter((id) => id !== pane);
  const index = before ? next.indexOf(before) : -1;
  next.splice(index >= 0 ? index : next.length, 0, pane);
  return next;
}

export function DockLayout({ center, panes }: { center: ReactNode; panes: DockPane[] }) {
  const order = useSettings((state) => state.dockPaneOrder);
  const sides = useSettings((state) => state.dockPaneSides);
  const treebarWidth = useSettings((state) => state.treebarWidth);
  const nicklistWidth = useSettings((state) => state.nicklistWidth);
  const panelsWidth = useSettings((state) => state.panelsWidth);
  const setSetting = useSettings((state) => state.set);
  const [dragged, setDragged] = useState<DockPaneId | null>(null);
  const paneMap = new Map(panes.map((pane) => [pane.id, pane]));
  const widths = { treebar: treebarWidth, nicklist: nicklistWidth, panels: panelsWidth };

  const dock = (pane: DockPaneId, side: DockSide, before?: DockPaneId) => {
    setSetting("dockPaneSides", { ...sides, [pane]: side });
    setSetting("dockPaneOrder", moveDockPane(order, pane, before));
    if (pane === "treebar") setSetting("treebarPosition", side);
    setDragged(null);
  };

  const resize = (event: PointerEvent, pane: DockPaneId, side: DockSide) => {
    event.preventDefault();
    const startX = event.clientX;
    const startWidth = widths[pane];
    const key = pane === "treebar" ? "treebarWidth" : pane === "nicklist" ? "nicklistWidth" : "panelsWidth";
    const minimum = pane === "nicklist" ? 120 : pane === "treebar" ? 140 : 160;
    const maximum = pane === "nicklist" ? 500 : 600;
    const onMove = (move: globalThis.PointerEvent) => {
      const delta = (move.clientX - startX) * (side === "left" ? 1 : -1);
      setSetting(key, Math.max(minimum, Math.min(maximum, startWidth + delta)));
    };
    const onUp = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      document.body.classList.remove("resizing-dock-pane");
    };
    document.body.classList.add("resizing-dock-pane");
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp, { once: true });
  };

  const renderSide = (side: DockSide) => {
    const visible = order.filter((id) => sides[id] === side && paneMap.has(id));
    return (
      <div
        className={`dock-zone dock-zone-${side}${dragged ? " dragging" : ""}`}
        onDragOver={(event) => event.preventDefault()}
        onDrop={() => dragged && dock(dragged, side)}
      >
        {visible.map((id) => {
          const pane = paneMap.get(id)!;
          return (
            <aside
              className={`dock-pane dock-pane-${id}`}
              key={id}
              style={{ "--dock-pane-width": `${widths[id]}px` } as CSSProperties}
              onDragOver={(event: DragEvent) => event.preventDefault()}
              onDrop={(event) => {
                event.stopPropagation();
                if (dragged) dock(dragged, side, id);
              }}
            >
              <div
                className="dock-pane-title"
                draggable
                onDragStart={() => setDragged(id)}
                onDragEnd={() => setDragged(null)}
                title="Drag to reposition this pane"
              >
                <span className="dock-grip" aria-hidden="true">⠿</span>
                <span>{pane.label}</span>
                <button
                  className="dock-side-button"
                  onClick={() => dock(id, side === "left" ? "right" : "left")}
                  title={`Dock ${pane.label} on the ${side === "left" ? "right" : "left"}`}
                >
                  {side === "left" ? "→" : "←"}
                </button>
              </div>
              <div className="dock-pane-content">{pane.content}</div>
              <div
                className={`dock-resizer dock-resizer-${side}`}
                onPointerDown={(event) => resize(event, id, side)}
                role="separator"
                aria-orientation="vertical"
                aria-label={`Resize ${pane.label}`}
              />
            </aside>
          );
        })}
        {dragged && visible.length === 0 && <div className="dock-drop-hint">Drop pane here</div>}
      </div>
    );
  };

  return <div className="dock-layout">{renderSide("left")}<div className="dock-center">{center}</div>{renderSide("right")}</div>;
}
