import { useEffect } from "react";
import { api } from "../lib/api";
import { useStore } from "../state/store";
import { OpenDialog, useDialogs } from "../state/dialogs";

/** Renders one open script dialog and reports interactions to the engine. */
function DialogView({ dialog }: { dialog: OpenDialog }) {
  const setValue = useDialogs((s) => s.setValue);
  const close = useDialogs((s) => s.close);
  const srv = useStore((s) => s.servers[dialog.serverId]);

  const fire = (control: string) => {
    api
      .scriptRunDialog(
        dialog.serverId,
        srv?.nick ?? "",
        srv?.name ?? "",
        dialog.name,
        control,
        dialog.values
      )
      .catch(() => {});
  };

  // Fire an `init` event once the dialog opens (so scripts can populate it).
  useEffect(() => {
    fire("init");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const onButton = (id: string, cancel: boolean) => {
    if (cancel) close(dialog.name);
    else fire(id);
  };

  return (
    <div className="modal-backdrop">
      <div className="modal dialog-modal" onClick={(e) => e.stopPropagation()}>
        <h2>{dialog.title || dialog.name}</h2>
        <div className="dialog-body">
          {dialog.controls.map((c) => {
            const val = dialog.values[c.id] ?? "";
            const opts = dialog.options[c.id] ?? c.options;
            switch (c.kind) {
              case "text":
                return <div key={c.id} className="dlg-text">{c.label}</div>;
              case "edit":
                return (
                  <input
                    key={c.id}
                    className="dlg-edit"
                    value={val}
                    onChange={(e) => setValue(dialog.name, c.id, e.target.value)}
                  />
                );
              case "editbox":
                return (
                  <textarea
                    key={c.id}
                    className="dlg-editbox"
                    value={val}
                    onChange={(e) => setValue(dialog.name, c.id, e.target.value)}
                  />
                );
              case "check":
                return (
                  <label key={c.id} className="dlg-check">
                    <input
                      type="checkbox"
                      checked={val === "1"}
                      onChange={(e) => setValue(dialog.name, c.id, e.target.checked ? "1" : "0")}
                    />
                    {c.label}
                  </label>
                );
              case "combo":
                return (
                  <select
                    key={c.id}
                    className="dlg-combo"
                    value={val}
                    onChange={(e) => setValue(dialog.name, c.id, e.target.value)}
                  >
                    {opts.map((o) => (
                      <option key={o} value={o}>
                        {o}
                      </option>
                    ))}
                  </select>
                );
              case "list":
                return (
                  <select
                    key={c.id}
                    className="dlg-list"
                    size={Math.min(Math.max(opts.length, 2), 8)}
                    value={val}
                    onChange={(e) => setValue(dialog.name, c.id, e.target.value)}
                  >
                    {opts.map((o) => (
                      <option key={o} value={o}>
                        {o}
                      </option>
                    ))}
                  </select>
                );
              default:
                return null;
            }
          })}
        </div>
        <div className="modal-actions">
          {dialog.controls
            .filter((c) => c.kind === "button")
            .map((b) => (
              <button
                key={b.id}
                className={b.default ? "primary" : b.cancel ? "ghost" : ""}
                onClick={() => onButton(b.id, b.cancel)}
              >
                {b.label}
              </button>
            ))}
        </div>
      </div>
    </div>
  );
}

/** Renders all currently-open script dialogs. */
export function UserDialogs() {
  const dialogs = useDialogs((s) => s.dialogs);
  return (
    <>
      {dialogs.map((d) => (
        <DialogView key={d.name} dialog={d} />
      ))}
    </>
  );
}
