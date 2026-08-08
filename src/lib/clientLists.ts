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

/** Shared shape for a list command: edits settings, returns the lines to echo. */
function listCommand(
  key: "ignores" | "notifyList",
  label: string,
  args: string,
  forceRemove = false
): string[] {
  const settings = useSettings.getState();
  const parts = args.trim().split(/\s+/).filter(Boolean);
  const remove = forceRemove || parts[0] === "-r";
  const who = remove && !forceRemove ? parts[1] : parts[0];
  const current = settings[key];

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

/** `/ignore [-r] [nick]` — no nick lists the current entries. */
export const runIgnore = (args: string) => listCommand("ignores", "Ignore", args);

/** `/unignore <nick>` — `/ignore -r` by another name. */
export const runUnignore = (args: string) =>
  args.trim() ? listCommand("ignores", "Ignore", args, true) : [];

/** `/notify [-r] [nick]` */
export const runNotify = (args: string) => listCommand("notifyList", "Notify", args);

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
