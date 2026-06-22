import { create } from "zustand";
import { IrcEvent } from "../lib/api";

/// Per-(server, nick) icon set by scripts via `/nickicon`. Icon is an emoji/text
/// glyph or an image URL.
interface NickIconState {
  icons: Record<string, string>;
}

const key = (serverId: string, nick: string) => `${serverId}|${nick.toLowerCase()}`;

export const useNickIcons = create<NickIconState>(() => ({ icons: {} }));

/** Routes nick-icon events into the store. */
export function routeNickIconEvent(ev: IrcEvent) {
  if (ev.type !== "nickIcon") return;
  useNickIcons.setState((s) => {
    const icons = { ...s.icons };
    const k = key(ev.serverId, ev.nick);
    if (ev.icon) icons[k] = ev.icon;
    else delete icons[k];
    return { icons };
  });
}

export const iconKey = key;
