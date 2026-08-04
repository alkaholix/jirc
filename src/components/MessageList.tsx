import { useEffect, useMemo, useRef, useState, type MouseEvent as ReactMouseEvent } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { Buffer, Line, useStore } from "../state/store";
import { useSettings, type TimestampMode } from "../state/settings";
import { api, PopupItem } from "../lib/api";
import { ContextMenu, PopupItems } from "./popupMenu";
import { parseIrc, stripFormatting, stripFormattingCodes } from "../ircFormat/parse";
import { nickColor } from "../lib/nickColor";
import { ircxDisplay } from "../lib/ircx";
import { FINDTEXT_COMMAND_EVENT, type FindTextCommand } from "../lib/clientCommands";

const isJoinPart = (l: Line) => l.kind === "event" && /^[→←]/.test(l.text);

function ts(line: Line): string {
  if (!line.ts) return "";
  const d = new Date(line.ts);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

/** Text of a line, used for search matching (nick + body, formatting stripped). */
function lineText(l: Line): string {
  return stripFormatting(`${l.from ?? ""} ${l.text}`).toLowerCase();
}

export function startsTimestampMinute(line: Line, previous?: Line): boolean {
  if (!line.ts) return false;
  return !previous?.ts || Math.floor(line.ts / 60000) !== Math.floor(previous.ts / 60000);
}

function Timestamp({
  line,
  mode,
  showDivider,
}: {
  line: Line;
  mode: TimestampMode;
  showDivider: boolean;
}) {
  const time = ts(line);
  if (!time || mode === "off") return null;
  if (mode === "divider" && !showDivider) return null;
  return mode === "divider" ? (
    <span className="timestamp-divider"><span>{time}</span></span>
  ) : (
    <span className="time">{time}</span>
  );
}

function LineRow({
  line,
  timestampMode,
  selfColor,
  showDivider,
  stripCodes,
}: {
  line: Line;
  timestampMode: TimestampMode;
  selfColor: string;
  showDivider: boolean;
  stripCodes: string;
}) {
  const text = stripFormattingCodes(line.text, stripCodes);
  const dividerClass =
    timestampMode === "divider" && showDivider ? " timestamp-divider-mode" : "";
  if (line.kind === "separator") {
    return <div className="line script-line-separator"><span>{line.text}</span></div>;
  }
  if (line.kind === "event" || line.kind === "system" || line.kind === "error") {
    return (
      <div className={`line line-${line.kind}${dividerClass}`}>
        <Timestamp line={line} mode={timestampMode} showDivider={showDivider} />
        <span className="meta">{parseIrc(text)}</span>
      </div>
    );
  }
  if (line.kind === "action") {
    return (
      <div className={`line line-action${dividerClass}`}>
        <Timestamp line={line} mode={timestampMode} showDivider={showDivider} />
        <span className="action-text">
          * <span style={{ color: nickColor(line.from ?? "", line.self, selfColor) }}>{ircxDisplay(line.from)}</span>{" "}
          {parseIrc(text)}
        </span>
      </div>
    );
  }
  if (line.kind === "whisper") {
    // Channel-scoped private message (IRCX) — visually distinct from channel text.
    const label = line.self
      ? `you whisper to ${ircxDisplay(line.to) || "?"}`
      : `${ircxDisplay(line.from)} whispers`;
    return (
      <div className={`line line-whisper${dividerClass}`}>
        <Timestamp line={line} mode={timestampMode} showDivider={showDivider} />
        <span className="whisper-text">
          <span className="whisper-mark">»</span> {label}: {parseIrc(text)}
        </span>
      </div>
    );
  }
  const isNotice = line.kind === "notice";
  return (
    <div className={`line line-msg${line.self ? " self" : ""}${dividerClass}`}>
      <Timestamp line={line} mode={timestampMode} showDivider={showDivider} />
      <span className="nick" style={{ color: nickColor(line.from ?? "", line.self, selfColor) }}>
        {isNotice ? "-" : ""}
        {ircxDisplay(line.from)}
        {isNotice ? "-" : ""}
      </span>
      <span className="text">{parseIrc(text)}</span>
    </div>
  );
}

export function MessageList({ buffer }: { buffer: Buffer }) {
  const parentRef = useRef<HTMLDivElement>(null);
  const stickRef = useRef(true);
  const timestampMode = useSettings((s) => s.timestampMode);
  const showJoinPart = useSettings((s) => s.showJoinPart);
  const selfColor = useSettings((s) => s.selfNickColor);
  const stripCodes = useSettings((s) => s.stripCodes);

  const [search, setSearch] = useState("");
  const [searchOpen, setSearchOpen] = useState(false);
  const [matchIdx, setMatchIdx] = useState(0);
  const findNextRef = useRef(true);
  const searchRef = useRef<HTMLInputElement>(null);

  const lines = useMemo(
    () => (showJoinPart ? buffer.lines : buffer.lines.filter((l) => !isJoinPart(l))),
    [buffer.lines, showJoinPart]
  );

  // Indices of lines matching the search query.
  const matches = useMemo(() => {
    const q = search.trim().toLowerCase();
    if (!q) return [] as number[];
    const out: number[] = [];
    lines.forEach((l, i) => {
      if (lineText(l).includes(q)) out.push(i);
    });
    return out;
  }, [lines, search]);

  const virtualizer = useVirtualizer({
    count: lines.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => 22,
    overscan: 20,
  });

  // Reset search when switching buffers.
  useEffect(() => {
    setSearch("");
    setSearchOpen(false);
  }, [buffer.key]);

  // Ctrl/Cmd+F opens search; Esc closes it.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "f") {
        e.preventDefault();
        setSearchOpen(true);
        setTimeout(() => searchRef.current?.focus(), 0);
      } else if (e.key === "Escape" && searchOpen) {
        setSearchOpen(false);
        setSearch("");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [searchOpen]);

  // Scroll to the current match.
  const current = matches.length ? matches[Math.min(matchIdx, matches.length - 1)] : -1;
  useEffect(() => {
    if (current >= 0) {
      stickRef.current = false;
      virtualizer.scrollToIndex(current, { align: "center" });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [current]);

  useEffect(() => {
    setMatchIdx(findNextRef.current ? 0 : Math.max(0, matches.length - 1));
    findNextRef.current = true;
  }, [search, matches.length]);

  useEffect(() => {
    const listener = (event: Event) => {
      const command = (event as CustomEvent<FindTextCommand>).detail;
      if (command.serverId !== buffer.serverId || command.target.toLowerCase() !== buffer.name.toLowerCase()) return;
      setSearchOpen(true);
      findNextRef.current = command.next;
      setSearch(command.text);
      setTimeout(() => searchRef.current?.focus(), 0);
    };
    window.addEventListener(FINDTEXT_COMMAND_EVENT, listener);
    return () => window.removeEventListener(FINDTEXT_COMMAND_EVENT, listener);
  }, [buffer.name, buffer.serverId]);

  const step = (dir: number) => {
    if (!matches.length) return;
    setMatchIdx((i) => (i + dir + matches.length) % matches.length);
  };

  // Track whether the user is pinned to the bottom.
  const onScroll = () => {
    const el = parentRef.current;
    if (!el) return;
    stickRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
  };

  useEffect(() => {
    if (stickRef.current && lines.length > 0) {
      virtualizer.scrollToIndex(lines.length - 1, { align: "end" });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [lines.length, buffer.key]);

  const matchSet = useMemo(() => new Set(matches), [matches]);

  // Right-click the window → the script-defined channel/query/status popup. Labels
  // are evaluated by the engine (empty ones dropped); the command runs through it.
  const server = useStore((s) => s.servers[buffer.serverId]);
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const [popups, setPopups] = useState<PopupItem[]>([]);
  const popupContext =
    buffer.kind === "window"
      ? buffer.name
      : buffer.kind === "channel"
        ? "channel"
        : buffer.kind === "query"
          ? "query"
          : "status";
  const popupTarget = buffer.kind === "status" ? "" : buffer.name;
  const openMenu = (e: ReactMouseEvent) => {
    e.preventDefault();
    api
      .scriptPopups(buffer.serverId, popupTarget, server?.nick ?? "", server?.name ?? "", popupContext, "")
      .then((items) => {
        const visible = buffer.kind === "window"
          ? items.filter((item) => !["mouse", "sclick", "dclick", "uclick", "rclick", "lbclick", "leave", "drop"].includes(item.label.trim().toLowerCase()))
          : items;
        setPopups(visible);
        if (visible.length) setMenu({ x: e.clientX, y: e.clientY });
      })
      .catch(() => {});
  };
  const runPopup = (item: PopupItem) => {
    api.scriptRunPopup(buffer.serverId, popupTarget, server?.nick ?? "", server?.name ?? "", item.command, [], undefined, item.source).catch(() => {});
    setMenu(null);
  };

  const selectWindowLine = (line: number, add: boolean) => {
    if (buffer.kind !== "window" || buffer.windowKind !== "listbox") return;
    api
      .scriptRunCommand(
        buffer.serverId,
        buffer.name,
        server?.nick ?? "",
        server?.name ?? "",
        "sline",
        `${add ? "-a " : ""}${buffer.name} ${line}`
      )
      .catch(() => {});
    api
      .scriptPopups(buffer.serverId, buffer.name, server?.nick ?? "", server?.name ?? "", buffer.name, "")
      .then((items) => {
        const event = items.find((item) => item.label.trim().toLowerCase() === "lbclick");
        if (event) {
          api.scriptWindowMouse(
            buffer.serverId, buffer.name, server?.nick ?? "", server?.name ?? "",
            event.command, event.source ?? "", 0, 0, line
          ).catch(() => {});
        }
      })
      .catch(() => {});
  };

  return (
    <div className="messages-wrap">
      {searchOpen && (
        <div className="search-bar">
          <input
            ref={searchRef}
            placeholder="Find in buffer…"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") step(e.shiftKey ? -1 : 1);
            }}
          />
          <span className="search-count">
            {matches.length ? `${Math.min(matchIdx, matches.length - 1) + 1}/${matches.length}` : "0/0"}
          </span>
          <button onClick={() => step(-1)} title="Previous">↑</button>
          <button onClick={() => step(1)} title="Next">↓</button>
          <button onClick={() => { setSearchOpen(false); setSearch(""); }} title="Close">×</button>
        </div>
      )}
      <div className="messages" ref={parentRef} onScroll={onScroll} onContextMenu={openMenu}>
        <div style={{ height: virtualizer.getTotalSize(), position: "relative", width: "100%" }}>
          {virtualizer.getVirtualItems().map((vi) => {
            const lineNumber = vi.index + 1;
            const selected =
              buffer.kind === "window" && (buffer.windowSelected ?? []).includes(lineNumber);
            const cls = matchSet.has(vi.index)
              ? vi.index === current
                ? " match current"
                : " match"
              : "";
            return (
              <div
                key={vi.key}
                data-index={vi.index}
                ref={virtualizer.measureElement}
                className={`msg-row${cls}${selected ? " window-selected" : ""}${
                  buffer.kind === "window" && buffer.windowKind === "listbox"
                    ? " window-list-row"
                    : ""
                }`}
                style={{ position: "absolute", top: 0, left: 0, width: "100%", transform: `translateY(${vi.start}px)` }}
                onClick={(event) => selectWindowLine(lineNumber, event.ctrlKey || event.metaKey)}
              >
                <LineRow
                  line={lines[vi.index]}
                  timestampMode={timestampMode}
                  selfColor={selfColor}
                  showDivider={startsTimestampMinute(lines[vi.index], lines[vi.index - 1])}
                  stripCodes={stripCodes}
                />
              </div>
            );
          })}
        </div>
      </div>
      {menu && (
        <ContextMenu x={menu.x} y={menu.y} onClose={() => setMenu(null)}>
          <PopupItems items={popups} onRun={runPopup} />
        </ContextMenu>
      )}
    </div>
  );
}
