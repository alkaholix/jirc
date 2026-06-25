import { useTransfers, Transfer } from "../state/transfers";

function pct(t: Transfer): number {
  if (t.status === "done") return 100;
  if (t.size <= 0) return 0;
  return Math.min(100, Math.round((t.transferred / t.size) * 100));
}

function human(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

/** A floating panel showing active and recent DCC file transfers with a bar. */
export function TransfersPanel() {
  const transfers = useTransfers((s) => s.transfers);
  const dismiss = useTransfers((s) => s.dismiss);
  const list = Object.values(transfers);
  if (list.length === 0) return null;
  return (
    <div className="transfers">
      <div className="transfers-head">DCC transfers</div>
      {list.map((t) => (
        <div key={t.id} className={`transfer ${t.status}`}>
          <div className="transfer-row">
            <span className="transfer-name" title={`${t.filename} (${t.nick})`}>
              {t.kind === "send" ? "↑" : "↓"} {t.filename}
            </span>
            <span className="transfer-meta">
              {t.status === "error"
                ? "failed"
                : `${human(t.transferred)} / ${human(t.size)} · ${pct(t)}%`}
            </span>
            <button className="ghost" title="Dismiss" onClick={() => dismiss(t.id)}>
              ✕
            </button>
          </div>
          <div className="progress">
            <div className={`progress-fill ${t.status}`} style={{ width: `${pct(t)}%` }} />
          </div>
        </div>
      ))}
    </div>
  );
}
