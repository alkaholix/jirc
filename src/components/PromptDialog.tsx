import { useEffect, useRef, useState } from "react";
import { usePrompt } from "../state/prompt";

/** In-app replacement for window.prompt(), styled like the confirm dialog. */
export function PromptDialog() {
  const request = usePrompt((s) => s.request);
  const respond = usePrompt((s) => s.respond);
  const [value, setValue] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    setValue(request?.initial ?? "");
    if (request) setTimeout(() => inputRef.current?.focus(), 0);
  }, [request]);

  if (!request) return null;

  return (
    <div className="modal-backdrop" onClick={() => respond(null)}>
      <div className="modal confirm-modal" onClick={(e) => e.stopPropagation()}>
        <h2>{request.title}</h2>
        {request.message && <p className="confirm-message">{request.message}</p>}
        <input
          ref={inputRef}
          className="prompt-input"
          value={value}
          placeholder={request.placeholder}
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") respond(value);
            if (e.key === "Escape") respond(null);
          }}
        />
        <div className="modal-actions">
          <button className="ghost" onClick={() => respond(null)}>
            Cancel
          </button>
          <button onClick={() => respond(value)}>{request.confirmLabel}</button>
        </div>
      </div>
    </div>
  );
}
