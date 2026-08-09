import { useEffect, useRef, useState } from "react";
import { usePrompt } from "../state/prompt";

/** Markers the script engine reads as "this button was pressed" rather than as
 *  entered text. The NUL prefix cannot occur in anything a user can type. */
const MARK = {
  ok: "\u0000ok",
  yes: "\u0000yes",
  no: "\u0000no",
  cancel: "\u0000cancel",
  retry: "\u0000retry",
  timeout: "\u0000timeout",
};

/** mIRC's `$input` icon letters. */
const ICONS: Record<string, string> = {
  t: "⭐",
  c: "🗑",
  i: "ℹ",
  q: "❓",
  w: "⚠",
  h: "⛔",
};

/** In-app replacement for window.prompt(), and the dialog behind `$input`.
 *
 *  The button set, entry field, icon and timeout all come from `$input`'s
 *  options; a plain `promptDialog()` call gets the defaults (a text box with
 *  OK/Cancel), which is what the rest of the app uses. */
export function PromptDialog() {
  const request = usePrompt((s) => s.request);
  const respond = usePrompt((s) => s.respond);
  const [value, setValue] = useState("");
  const [remaining, setRemaining] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    setValue(request?.initial ?? "");
    setRemaining(request?.timeoutSecs ?? 0);
    if (request) setTimeout(() => inputRef.current?.focus(), 0);
  }, [request]);

  // `kN` — count down and answer with the timeout marker if nobody acts.
  useEffect(() => {
    if (!request?.timeoutSecs) return;
    const id = setInterval(() => {
      setRemaining((left) => {
        if (left <= 1) {
          clearInterval(id);
          respond(MARK.timeout);
          return 0;
        }
        return left - 1;
      });
    }, 1000);
    return () => clearInterval(id);
  }, [request]);

  if (!request) return null;

  const hasField = request.field !== "none";
  // With a field the leftmost button submits its contents; without one, the
  // answer *is* which button was pressed.
  const submit = () => respond(hasField ? value : MARK.ok);
  const glyph = ICONS[request.icon];

  const buttons = () => {
    switch (request.buttons) {
      case "yesno":
        return [
          { label: "Yes", value: hasField ? value : MARK.yes, primary: true },
          { label: "No", value: MARK.no, primary: false },
        ];
      case "yesnocancel":
        return [
          { label: "Yes", value: hasField ? value : MARK.yes, primary: true },
          { label: "No", value: MARK.no, primary: false },
          { label: "Cancel", value: MARK.cancel, primary: false },
        ];
      case "retrycancel":
        return [
          { label: "Retry", value: hasField ? value : MARK.retry, primary: true },
          { label: "Cancel", value: MARK.cancel, primary: false },
        ];
      default:
        return [
          { label: "OK", value: hasField ? value : MARK.ok, primary: true },
          // A lone OK is modal-with-no-escape in mIRC only when it has no
          // field; keep Cancel for the text forms so Esc has a match.
          ...(hasField ? [{ label: "Cancel", value: MARK.cancel, primary: false }] : []),
        ];
    }
  };

  return (
    <div className="modal-backdrop" onClick={() => respond(MARK.cancel)}>
      <div className="modal confirm-modal" onClick={(e) => e.stopPropagation()}>
        <h2>
          {glyph && <span className="prompt-icon">{glyph}</span>}
          {request.title}
        </h2>
        {request.message && <p className="confirm-message">{request.message}</p>}
        {request.field === "combo" ? (
          <select
            className="prompt-input"
            value={value}
            onChange={(e) => setValue(e.target.value)}
          >
            {request.items.map((item) => (
              <option key={item} value={item}>
                {item}
              </option>
            ))}
          </select>
        ) : hasField ? (
          <input
            ref={inputRef}
            className="prompt-input"
            type={request.field === "password" ? "password" : "text"}
            value={value}
            placeholder={request.placeholder}
            onChange={(e) => setValue(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") submit();
              if (e.key === "Escape") respond(MARK.cancel);
            }}
          />
        ) : null}
        {remaining > 0 && <p className="prompt-timeout">Closing in {remaining}s</p>}
        <div className="modal-actions">
          {buttons()
            .slice()
            .reverse()
            .map((b) => (
              <button
                key={b.label}
                className={b.primary ? undefined : "ghost"}
                onClick={() => respond(b.value)}
              >
                {b.label}
              </button>
            ))}
        </div>
      </div>
    </div>
  );
}
