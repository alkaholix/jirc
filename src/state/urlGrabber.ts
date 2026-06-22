import { create } from "zustand";
import { IrcEvent } from "../lib/api";

export interface GrabbedUrl {
  url: string;
  from: string;
  buffer: string;
}

interface UrlState {
  urls: GrabbedUrl[];
  clear: () => void;
}

/** mIRC-style URL grabber: a rolling list of links seen in chat. */
export const useUrlGrabber = create<UrlState>((set) => ({
  urls: [],
  clear: () => set({ urls: [] }),
}));

const URL_RE = /\bhttps?:\/\/[^\s<>"']+/gi;
const MAX_URLS = 100;

/** Captures any URLs in a message event into the grabber list (deduped per buffer). */
export function routeUrlEvent(ev: IrcEvent): void {
  if (ev.type !== "message") return;
  const matches = ev.text.match(URL_RE);
  if (!matches) return;
  const from = ev.from ?? "?";
  const buffer = ev.target;
  useUrlGrabber.setState((s) => {
    const next = [...s.urls];
    for (const url of matches) {
      if (!next.some((u) => u.url === url && u.buffer === buffer)) next.push({ url, from, buffer });
    }
    return { urls: next.slice(-MAX_URLS) };
  });
}
