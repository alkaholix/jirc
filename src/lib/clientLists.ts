// Client-side list commands (`/ignore`, `/notify`, `/urls`).
//
// These edit state that lives only in the frontend, so they have no server verb
// behind them. They used to exist solely in `slash.ts`, which the typed input
// bar reaches and scripts do not — a scripted `/ignore bob` therefore fell
// through the script engine's unknown-command fallback and sent a literal
// `IGNORE bob` to the server: silently doing nothing locally while telling the
// network what the user meant to do.
//
// Keeping the logic here means both entry points run the same code rather than
// two copies that drift.

import { useSettings } from "../state/settings";
import { useUrlGrabber } from "../state/urlGrabber";

const has = (list: string[], value: string) =>
  list.some((entry) => entry.toLowerCase() === value.toLowerCase());
const without = (list: string[], value: string) =>
  list.filter((entry) => entry.toLowerCase() !== value.toLowerCase());

/** A leading `-switches` token, and the arguments after it. */
export function splitSwitches(args: string): { flags: string; rest: string[] } {
  const parts = args.trim().split(/\s+/).filter(Boolean);
  if (parts[0]?.startsWith("-") && parts[0].length > 1) {
    return { flags: parts[0].slice(1).toLowerCase(), rest: parts.slice(1) };
  }
  return { flags: "", rest: parts };
}

/** Edits the stored list and returns the lines to echo. */
function applyList(
  key: "ignores" | "notifyList",
  label: string,
  who: string | undefined,
  remove: boolean,
  removeAll = false
): string[] {
  const settings = useSettings.getState();
  const current = settings[key];
  if (removeAll) {
    settings.set(key, []);
    return [`${label} list cleared.`];
  }
  if (!who) {
    return [`${label} list: ${current.length ? current.join(", ") : "(none)"}`];
  }
  if (remove) {
    settings.set(key, without(current, who));
    return [`${who} removed from ${label.toLowerCase()}`];
  }
  if (!has(current, who)) settings.set(key, [...current, who]);
  return [`${who} added to ${label.toLowerCase()}`];
}

/**
 * mIRC's `/ignore` type switches. jIRC's ignore list is all-or-nothing — one
 * `isIgnored(nick)` test with no notion of message type — so any of these
 * selects the whole user. `-a` means exactly that in mIRC too; the narrower
 * ones ask for something jIRC cannot do, and say so rather than quietly
 * over-ignoring.
 */
const IGNORE_TYPES = "pcntikdshyf";

/** `/ignore [-rlpcntikdshyfa] [nick]` — no nick lists the current entries. */
export function runIgnore(args: string): string[] {
  const { flags, rest } = splitSwitches(args);
  // `-r` with no address removes every ignore, which ignore.rst documents.
  if (flags.includes("r")) {
    return applyList("ignores", "Ignore", rest[0], true, rest.length === 0);
  }
  if (flags.includes("l") || rest.length === 0) {
    return applyList("ignores", "Ignore", undefined, false);
  }
  const narrowed = [...flags].filter((f) => IGNORE_TYPES.includes(f)).join("");
  const lines = applyList("ignores", "Ignore", rest[0], false);
  if (narrowed && !flags.includes("a")) {
    lines.push(
      `(jIRC's ignore is all-or-nothing, so -${narrowed} ignores ${rest[0]} entirely.)`
    );
  }
  return lines;
}

/** `/unignore <nick>` — `/ignore -r` by another name. */
export const runUnignore = (args: string) => {
  const who = args.trim().split(/\s+/)[0];
  return who ? applyList("ignores", "Ignore", who, true) : [];
};

/**
 * `/notify [-shrln] [network] [nick]`. `-s`/`-h` show and hide mIRC's notify
 * *window*, which jIRC has no equivalent of — its list lives in Settings — so
 * they are consumed. `-n` says a network was given; jIRC's list is not
 * per-network, so the network token is consumed and the nick still parses.
 */
export function runNotify(args: string): string[] {
  const { flags, rest } = splitSwitches(args);
  const who = flags.includes("n") ? rest[1] : rest[0];
  if (flags.includes("r")) {
    return applyList("notifyList", "Notify", who, true);
  }
  if (flags.includes("l") || !who) {
    return applyList("notifyList", "Notify", undefined, false);
  }
  return applyList("notifyList", "Notify", who, false);
}

/** `/urls [clear]` — the URL grabber's catch list. */
export function runUrls(args: string): string[] {
  const grabber = useUrlGrabber.getState();
  if (args.trim().toLowerCase() === "clear") {
    grabber.clear();
    return ["URL list cleared."];
  }
  const urls = grabber.urls;
  if (!urls.length) return ["No URLs captured yet."];
  return [
    `Captured URLs (${urls.length}, newest last) — /urls clear to reset:`,
    ...urls.slice(-25).map((u) => `  ${u.url}  — ${u.from} in ${u.buffer}`),
  ];
}
