import { useEffect } from "react";
import { useConfirm } from "../state/confirm";

export function ConfirmDialog() {
  const request = useConfirm((s) => s.request);
  const respond = useConfirm((s) => s.respond);

  useEffect(() => {
    if (!request) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") respond(false);
      if (e.key === "Enter") respond(true);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [request, respond]);

  if (!request) return null;

  return (
    <div className="modal-backdrop" onClick={() => respond(false)}>
      <div className="modal confirm-modal" onClick={(e) => e.stopPropagation()}>
        <h2>{request.title}</h2>
        <p className="confirm-message">{request.message}</p>
        <div className="modal-actions">
          <button className="ghost" onClick={() => respond(false)}>
            Cancel
          </button>
          <button className={request.danger ? "danger-btn" : ""} onClick={() => respond(true)} autoFocus>
            {request.confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
