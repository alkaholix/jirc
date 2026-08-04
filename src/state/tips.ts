import { notify } from "../lib/notify";

export interface ActiveTip {
  name: string;
  title: string;
  text: string;
  expiresAt: number;
  alias: string;
  wid: string;
  serverId: string;
  target: string;
}

export const TIPS_CHANGED_EVENT = "jirc-tips-changed";

const active = new Map<string, ActiveTip>();
const timers = new Map<string, number>();

const keyFor = (name: string) => name.toLowerCase();
const changed = () => window.dispatchEvent(new Event(TIPS_CHANGED_EVENT));

function schedule(tip: ActiveTip) {
  const key = keyFor(tip.name);
  const old = timers.get(key);
  if (old !== undefined) window.clearTimeout(old);
  timers.set(key, window.setTimeout(() => {
    active.delete(key);
    timers.delete(key);
    changed();
  }, Math.max(0, tip.expiresAt - Date.now())));
}

export function activeTips(): ActiveTip[] {
  return [...active.values()];
}

export function clearTips() {
  for (const timer of timers.values()) window.clearTimeout(timer);
  timers.clear();
  active.clear();
  changed();
}

export function routeTipCommand(
  command: string,
  args: string,
  context: { serverId: string; target: string } = { serverId: "", target: "" }
) {
  const fields = args.split("\u001f");
  if (command === "tip-create") {
    const [name, title, text, delayRaw, , , alias = "", wid = ""] = fields;
    if (!name) return;
    const delay = Math.max(3, Math.min(60, Number(delayRaw) || 10));
    const tip = {
      name, title, text, expiresAt: Date.now() + delay * 1000,
      alias, wid, serverId: context.serverId, target: context.target,
    };
    active.set(keyFor(name), tip);
    schedule(tip);
    void notify(title || name, text);
    changed();
  } else if (command === "tip-close") {
    const key = keyFor(args);
    const timer = timers.get(key);
    if (timer !== undefined) window.clearTimeout(timer);
    timers.delete(key);
    active.delete(key);
    changed();
  } else if (command === "tip-update") {
    const [name, text] = fields;
    const key = keyFor(name);
    const tip = active.get(key);
    if (!tip) return;
    const updated = { ...tip, text };
    active.set(key, updated);
    void notify(updated.title || updated.name, updated.text);
    changed();
  }
}
