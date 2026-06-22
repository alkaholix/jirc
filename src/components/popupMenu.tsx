import { ReactNode, useLayoutEffect, useRef, useState } from "react";
import { PopupItem } from "../lib/api";

/** A popup item with children — its flyout opens left when it would otherwise run
 *  off the right edge of the window. (Labels arrive already evaluated from the
 *  engine, so this is pure rendering.) */
export function SubMenu({ item, onRun }: { item: PopupItem; onRun: (command: string) => void }) {
  const subRef = useRef<HTMLDivElement>(null);
  const [flipLeft, setFlipLeft] = useState(false);

  const onEnter = () => {
    const sub = subRef.current;
    const parent = sub?.parentElement;
    if (!sub || !parent) return;
    const rect = parent.getBoundingClientRect();
    setFlipLeft(rect.right + sub.offsetWidth > window.innerWidth - 8);
  };

  return (
    <div className="pmenu-item has-sub" onMouseEnter={onEnter}>
      <span className="pmenu-label">
        {item.label} <span className="pmenu-arrow">▸</span>
      </span>
      <div ref={subRef} className={`pmenu-sub context-menu${flipLeft ? " flip-left" : ""}`}>
        <PopupItems items={item.children} onRun={onRun} />
      </div>
    </div>
  );
}

/** Recursively renders script-defined popup items. */
export function PopupItems({ items, onRun }: { items: PopupItem[]; onRun: (command: string) => void }) {
  return (
    <>
      {items.map((item, i) =>
        item.separator ? (
          <div key={i} className="menu-sep" />
        ) : item.children.length > 0 ? (
          <SubMenu key={i} item={item} onRun={onRun} />
        ) : (
          <button key={i} onClick={() => onRun(item.command)}>
            {item.label}
          </button>
        )
      )}
    </>
  );
}

/** A positioned right-click menu shell: a full-screen backdrop that closes on
 *  click, plus the menu box clamped to stay inside the viewport. */
export function ContextMenu({
  x,
  y,
  onClose,
  children,
}: {
  x: number;
  y: number;
  onClose: () => void;
  children: ReactNode;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: x, top: y });

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const margin = 8;
    const rect = el.getBoundingClientRect();
    let left = x;
    let top = y;
    if (left + rect.width > window.innerWidth - margin) left = window.innerWidth - rect.width - margin;
    if (top + rect.height > window.innerHeight - margin) top = window.innerHeight - rect.height - margin;
    setPos({ left: Math.max(margin, left), top: Math.max(margin, top) });
  }, [x, y]);

  return (
    <>
      <div
        className="menu-backdrop"
        onClick={onClose}
        onContextMenu={(e) => {
          e.preventDefault();
          onClose();
        }}
      />
      <div ref={ref} className="context-menu" style={{ left: pos.left, top: pos.top }}>
        {children}
      </div>
    </>
  );
}
