import { useEffect, useState } from "react";
import { api } from "../lib/api";
import { bufferKey } from "../state/store";
import { useChannelModes, useChannelCentral } from "../state/channelModes";

const FLAGS: [string, string][] = [
  ["m", "Moderated (+m) — only ops/voiced talk"],
  ["t", "Topic locked (+t) — ops set the topic"],
  ["i", "Invite only (+i)"],
  ["n", "No external messages (+n)"],
  ["s", "Secret (+s)"],
  ["p", "Private (+p)"],
];

/** mIRC-style Channel Central: view & set the active channel's modes. */
export function ChannelCentral() {
  const target = useChannelCentral((s) => s.target);
  const close = useChannelCentral((s) => s.close);
  const key = target ? bufferKey(target.serverId, target.channel) : "";
  const current = useChannelModes((s) => (key ? s.byBuffer[key] : undefined));

  const [flags, setFlags] = useState<Set<string>>(new Set());
  const [chKey, setChKey] = useState("");
  const [limit, setLimit] = useState("");

  // Ask the server for the current modes when the dialog opens.
  useEffect(() => {
    if (target) api.sendRaw(target.serverId, `MODE ${target.channel}`).catch(() => {});
  }, [target?.serverId, target?.channel]);

  // Seed the editable copy from the tracked modes (incl. the 324 reply).
  useEffect(() => {
    setFlags(new Set(current?.flags ?? []));
    setChKey(current?.key ?? "");
    setLimit(current?.limit ?? "");
  }, [current]);

  if (!target) return null;

  const toggle = (f: string) =>
    setFlags((s) => {
      const next = new Set(s);
      if (next.has(f)) next.delete(f);
      else next.add(f);
      return next;
    });

  const apply = async () => {
    const cur = current ?? { flags: new Set<string>(), key: "", limit: "" };
    const plus: string[] = [];
    const minus: string[] = [];
    const params: string[] = [];
    for (const [f] of FLAGS) {
      if (flags.has(f) && !cur.flags.has(f)) plus.push(f);
      else if (!flags.has(f) && cur.flags.has(f)) minus.push(f);
    }
    if (chKey !== cur.key) {
      if (chKey) {
        plus.push("k");
        params.push(chKey);
      } else if (cur.key) {
        minus.push("k");
        params.push(cur.key);
      }
    }
    if (limit !== cur.limit) {
      if (limit) {
        plus.push("l");
        params.push(limit);
      } else if (cur.limit) {
        minus.push("l");
      }
    }
    const modeStr = (plus.length ? "+" + plus.join("") : "") + (minus.length ? "-" + minus.join("") : "");
    if (modeStr) {
      await api.sendRaw(target.serverId, `MODE ${target.channel} ${modeStr} ${params.join(" ")}`.trim());
    }
    close();
  };

  return (
    <div className="modal-backdrop" onClick={close}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <h2>Channel Central — {target.channel}</h2>
        <div className="modal-body settings-body">
          {FLAGS.map(([f, label]) => (
            <label key={f} className="inline">
              <input type="checkbox" checked={flags.has(f)} onChange={() => toggle(f)} />
              {label}
            </label>
          ))}
          <div className="row">
            <label className="grow">
              Key (+k)
              <input value={chKey} onChange={(e) => setChKey(e.target.value)} placeholder="none" />
            </label>
            <label>
              Limit (+l)
              <input
                type="number"
                min={0}
                value={limit}
                onChange={(e) => setLimit(e.target.value)}
                placeholder="none"
              />
            </label>
          </div>
        </div>
        <div className="modal-actions">
          <button onClick={close}>Cancel</button>
          <button onClick={apply}>Apply</button>
        </div>
      </div>
    </div>
  );
}
