import { useEffect, useState } from "react";
import { api } from "../lib/api";
import { activeTips, routeTipCommand, TIPS_CHANGED_EVENT, type ActiveTip } from "../state/tips";
import { useStore } from "../state/store";

export function TipOverlay() {
  const [tips, setTips] = useState<ActiveTip[]>(activeTips);
  useEffect(() => {
    const refresh = () => setTips(activeTips());
    window.addEventListener(TIPS_CHANGED_EVENT, refresh);
    return () => window.removeEventListener(TIPS_CHANGED_EVENT, refresh);
  }, []);

  const activate = (tip: ActiveTip) => {
    if (!tip.alias || !tip.serverId) return;
    const server = useStore.getState().servers[tip.serverId];
    void api.scriptRunAlias(
      tip.serverId,
      tip.target,
      server?.nick ?? "",
      server?.name ?? "",
      tip.alias,
      ""
    );
    routeTipCommand("tip-close", tip.name);
  };

  if (!tips.length) return null;
  return <div className="tip-overlay" aria-live="polite">
    {tips.map((tip) => <div
      key={tip.name.toLowerCase()}
      className={`script-tip${tip.alias ? " clickable" : ""}`}
      title={tip.alias ? `Double-click to run ${tip.alias}` : undefined}
      onDoubleClick={() => activate(tip)}
    >
      <button aria-label={`Close ${tip.title || tip.name}`} onClick={() => routeTipCommand("tip-close", tip.name)}>×</button>
      <strong>{tip.title || tip.name}</strong>
      <span>{tip.text}</span>
    </div>)}
  </div>;
}
