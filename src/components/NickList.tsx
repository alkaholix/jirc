import { MouseEvent, useEffect, useLayoutEffect, useRef, useState } from "react";
import { Buffer, useStore } from "../state/store";
import { useSettings } from "../state/settings";
import { api, PopupItem } from "../lib/api";
import { nickColor } from "../lib/nickColor";
import { ircxDisplay } from "../lib/ircx";
import { handleInput } from "../lib/slash";
import { promptDialog } from "../state/prompt";
import { iconKey, useNickIcons } from "../state/nickIcons";
import { open } from "@tauri-apps/plugin-dialog";

const isUrl = (s: string) => /^(https?:|data:)/i.test(s);

interface MenuState {
  nick: string;
  x: number;
  y: number;
}

/** A popup item with children — its flyout opens left when it would otherwise
 *  run off the right edge of the window. */
function SubMenu({ item, onRun }: { item: PopupItem; onRun: (command: string) => void }) {
  const subRef = useRef<HTMLDivElement>(null);
  const [flipLeft, setFlipLeft] = useState(false);

  // Decide direction from the parent item + submenu width, independent of the
  // current flip state (so it can't oscillate on repeated hovers).
  const onEnter = () => {
    const sub = subRef.current;
    const item = sub?.parentElement;
    if (!sub || !item) return;
    const rect = item.getBoundingClientRect();
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
function PopupItems({ items, onRun }: { items: PopupItem[]; onRun: (command: string) => void }) {
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

export function NickList({ buffer }: { buffer: Buffer }) {
  const ensureBuffer = useStore((s) => s.ensureBuffer);
  const setActive = useStore((s) => s.setActive);
  const server = useStore((s) => s.servers[buffer.serverId]);
  const selfColor = useSettings((s) => s.selfNickColor);
  const nickIcons = useNickIcons((s) => s.icons);
  const [menu, setMenu] = useState<MenuState | null>(null);
  const [pos, setPos] = useState({ left: 0, top: 0 });
  const [popups, setPopups] = useState<PopupItem[]>([]);
  const menuRef = useRef<HTMLDivElement>(null);

  const { serverId, name: channel } = buffer;
  const prefixes = server?.prefixes ?? "~&@%+";
  // Colour each prefix level by its rank (highest first).
  const RANK_COLORS = ["#e0af68", "#f7768e", "#9ece6a", "#7dcfff", "#bb9af7"];
  const prefixColor = (p: string) => {
    const i = p ? prefixes.indexOf(p[0]) : -1;
    return i >= 0 ? RANK_COLORS[Math.min(i, RANK_COLORS.length - 1)] : undefined;
  };

  // Load user-defined nicklist popups (reloads cheaply when the menu opens, so
  // dynamic labels re-evaluate). $1 = the right-clicked nick.
  useEffect(() => {
    if (menu)
      api
        .scriptPopups(serverId, channel, server?.nick ?? "", server?.name ?? "", "nicklist", menu.nick)
        .then(setPopups)
        .catch(() => setPopups([]));
  }, [menu]);

  // Keep the menu fully on-screen: clamp it inside the window once its real size
  // is known (runs before paint, so there's no flicker). Re-runs when the menu
  // opens or its contents change height (popups loading).
  useLayoutEffect(() => {
    if (!menu || !menuRef.current) return;
    const margin = 8;
    const rect = menuRef.current.getBoundingClientRect();
    let left = menu.x;
    let top = menu.y;
    if (left + rect.width > window.innerWidth - margin) left = window.innerWidth - rect.width - margin;
    if (top + rect.height > window.innerHeight - margin) top = window.innerHeight - rect.height - margin;
    setPos({ left: Math.max(margin, left), top: Math.max(margin, top) });
  }, [menu, popups]);

  const openQuery = (nick: string) => {
    setActive(ensureBuffer(serverId, nick, "query"));
    setMenu(null);
  };

  const raw = (line: string) => {
    api.sendRaw(serverId, line).catch(() => {});
    setMenu(null);
  };

  const ignore = (nick: string) => {
    const st = useSettings.getState();
    if (!st.ignores.includes(nick)) st.set("ignores", [...st.ignores, nick]);
    setMenu(null);
  };

  const whisper = async (nick: string) => {
    setMenu(null);
    const text = await promptDialog(`Private message to ${ircxDisplay(nick)} in ${ircxDisplay(channel)}:`, {
      title: "Whisper",
      confirmLabel: "Send",
    });
    if (text) handleInput(`/whisper ${nick} ${text}`, buffer);
  };

  const kickWithReason = async (nick: string) => {
    setMenu(null);
    const reason = await promptDialog(`Reason for kicking ${ircxDisplay(nick)}:`, {
      title: "Kick",
      placeholder: "(optional reason)",
      confirmLabel: "Kick",
    });
    if (reason !== null) {
      const line = reason ? `KICK ${channel} ${nick} :${reason}` : `KICK ${channel} ${nick}`;
      api.sendRaw(serverId, line).catch(() => {});
    }
  };

  const dccChat = (nick: string) => {
    api.dccChat(serverId, nick).catch(() => {});
    setMenu(null);
  };

  const dccSend = async (nick: string) => {
    setMenu(null);
    const picked = await open({ multiple: false, title: `Send a file to ${nick}` }).catch(
      () => null
    );
    if (typeof picked === "string") api.dccSendFile(serverId, nick, picked).catch(() => {});
  };

  // Runs a script popup command with $1 = selected nick, $chan = channel.
  const runPopup = (command: string, nick: string) => {
    api
      .scriptRunPopup(serverId, channel, server?.nick ?? "", server?.name ?? "", command, [nick])
      .catch(() => {});
    setMenu(null);
  };

  const openMenu = (e: MouseEvent, nick: string) => {
    e.preventDefault();
    setMenu({ nick, x: e.clientX, y: e.clientY });
    // Seed at the cursor; the layout effect refines it before paint.
    setPos({ left: e.clientX, top: e.clientY });
  };

  return (
    <aside className="nicklist">
      <div className="nicklist-header">{buffer.members.length} users</div>
      <div className="nicklist-body">
        {buffer.members.map((m) => {
          const icon = nickIcons[iconKey(serverId, m.nick)];
          return (
          <div
            key={m.nick}
            className="nick-entry"
            title={m.nick}
            onDoubleClick={() => openQuery(m.nick)}
            onContextMenu={(e) => openMenu(e, m.nick)}
          >
            <span className="prefix" style={{ color: prefixColor(m.prefix) }}>
              {m.prefix[0] ?? " "}
            </span>
            {icon &&
              (isUrl(icon) ? (
                <img className="nick-icon" src={icon} alt="" />
              ) : (
                <span className="nick-icon">{icon}</span>
              ))}
            <span style={{ color: nickColor(m.nick, m.nick === server?.nick, selfColor) }}>{ircxDisplay(m.nick)}</span>
          </div>
          );
        })}
      </div>

      {menu && (
        <>
          <div
            className="menu-backdrop"
            onClick={() => setMenu(null)}
            onContextMenu={(e) => {
              e.preventDefault();
              setMenu(null);
            }}
          />
          <div ref={menuRef} className="context-menu" style={{ left: pos.left, top: pos.top }}>
            <div className="menu-title">{ircxDisplay(menu.nick)}</div>
            {popups.length > 0 ? (
              <PopupItems items={popups} onRun={(cmd) => runPopup(cmd, menu.nick)} />
            ) : (
              <>
                <button onClick={() => raw(`WHOIS ${menu.nick}`)}>Whois</button>
                <button onClick={() => openQuery(menu.nick)}>Query</button>
                <button onClick={() => whisper(menu.nick)}>Whisper…</button>
                <div className="menu-sep" />
                <button onClick={() => dccChat(menu.nick)}>DCC Chat</button>
                <button onClick={() => dccSend(menu.nick)}>DCC Send File…</button>
                <div className="menu-sep" />
                <button onClick={() => raw(`MODE ${channel} +q ${menu.nick}`)}>Owner (+q)</button>
                <button onClick={() => raw(`MODE ${channel} -q ${menu.nick}`)}>Deowner (-q)</button>
                <button onClick={() => raw(`MODE ${channel} +o ${menu.nick}`)}>Op (+o)</button>
                <button onClick={() => raw(`MODE ${channel} -o ${menu.nick}`)}>Deop (-o)</button>
                <button onClick={() => raw(`MODE ${channel} +v ${menu.nick}`)}>Voice (+v)</button>
                <button onClick={() => raw(`MODE ${channel} -v ${menu.nick}`)}>Devoice (-v)</button>
                <div className="menu-sep" />
                <button onClick={() => raw(`KICK ${channel} ${menu.nick}`)}>Kick</button>
                <button onClick={() => kickWithReason(menu.nick)}>Kick with message…</button>
                <button className="danger" onClick={() => raw(`MODE ${channel} +b ${menu.nick}!*@*`)}>
                  Ban
                </button>
                <div className="menu-sep" />
                <button onClick={() => ignore(menu.nick)}>Ignore</button>
              </>
            )}
          </div>
        </>
      )}
    </aside>
  );
}
