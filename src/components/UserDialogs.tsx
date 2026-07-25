import { useEffect, useState, type KeyboardEvent } from "react";
import { api, type DialogControl } from "../lib/api";
import { useStore } from "../state/store";
import { OpenDialog, useDialogs } from "../state/dialogs";

function DialogIcon({ filename, label }: { filename: string; label: string }) {
  const [source, setSource] = useState("");
  useEffect(() => {
    api.scriptPictureRead(filename).then(setSource).catch(() => setSource(""));
  }, [filename]);
  return source ? <img className="dlg-icon" src={source} alt={label} /> : null;
}

/** Renders one open script dialog and reports interactions to the engine. */
function DialogView({ dialog }: { dialog: OpenDialog }) {
  const setValue = useDialogs((s) => s.setValue);
  const close = useDialogs((s) => s.close);
  const srv = useStore((s) => s.servers[dialog.serverId]);

  const fire = (event: string, control: string, values = dialog.values) => {
    const snapshot = { ...values };
    for (const [id, options] of Object.entries(dialog.options)) {
      snapshot[`\u0000options\u0000${id}`] = options.join("\n");
    }
    dialog.controls.forEach((item, index) => {
      snapshot[`\u0000enabled\u0000${item.id}`] = String(item.enabled);
      snapshot[`\u0000visible\u0000${item.id}`] = String(item.visible);
      snapshot[`\u0000edited\u0000${item.id}`] = String(dialog.edited[item.id] ?? false);
      snapshot[`\u0000next\u0000${item.id}`] = dialog.controls[index + 1]?.id ?? "";
      snapshot[`\u0000prev\u0000${item.id}`] = dialog.controls[index - 1]?.id ?? "";
    });
    return api
      .scriptRunDialog(
        dialog.serverId,
        srv?.nick ?? "",
        srv?.name ?? "",
        dialog.name,
        event,
        control,
        snapshot
      )
      .catch(() => false);
  };

  // Fire an `init` event once the dialog opens (so scripts can populate it).
  useEffect(() => {
    fire("init", "0");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const change = (id: string, value: string, event = "edit") => {
    const values = { ...dialog.values, [id]: value };
    setValue(dialog.name, id, value);
    fire(event, id, values);
  };

  const closeDialog = async () => {
    await fire("close", "0");
    close(dialog.name);
  };

  const selectRadio = (control: DialogControl) => {
    const values = { ...dialog.values };
    for (const item of dialog.controls) {
      if (item.kind === "radio" && item.tab === control.tab) {
        values[item.id] = item.id === control.id ? "1" : "0";
        setValue(dialog.name, item.id, values[item.id]);
      }
    }
    fire("sclick", control.id, values);
  };

  const onButton = async (id: string, cancel: boolean, ok: boolean) => {
    const halted = await fire("sclick", id);
    if ((cancel || ok) && !halted) await closeDialog();
  };

  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key === "Escape") {
      event.preventDefault();
      const cancel = dialog.controls.find((control) => control.cancel);
      if (cancel) onButton(cancel.id, true, false);
      else closeDialog();
    } else if (event.key === "Enter" && !(event.target instanceof HTMLTextAreaElement)) {
      const button = dialog.controls.find((control) => control.default);
      if (button) {
        event.preventDefault();
        onButton(button.id, button.cancel, button.ok);
      }
    }
  };

  return (
    <div className="modal-backdrop">
      <div
        className="modal dialog-modal"
        style={{
          width: dialog.width > 0 ? dialog.width : undefined,
          minHeight: dialog.height > 0 ? dialog.height : undefined,
          resize: "both",
          overflow: "auto",
        }}
        onClick={(e) => e.stopPropagation()}
        onKeyDown={onKeyDown}
      >
        <h2>{dialog.title || dialog.name}</h2>
        {dialog.controls.some((control) => control.kind === "menu" && !control.tab) && (
          <nav className="dlg-menu">
            {dialog.controls.filter((control) => control.kind === "menu" && !control.tab && control.visible).map((menu) => (
              <details key={menu.id}>
                <summary>{menu.label}</summary>
                {dialog.controls.filter((item) => item.kind === "item" && item.tab === menu.id && item.visible).map((item) =>
                  item.label.toLowerCase() === "break"
                    ? <hr key={item.id} />
                    : <button key={item.id} disabled={!item.enabled} onClick={() => fire("menu", item.id)}>{item.label}</button>
                )}
              </details>
            ))}
          </nav>
        )}
        {dialog.controls.some((control) => control.kind === "tab") && (
          <div className="dlg-tabs" role="tablist">
            {dialog.controls.filter((control) => control.kind === "tab" && control.visible).map((tab) => (
              <button
                key={tab.id}
                role="tab"
                disabled={!tab.enabled}
                aria-selected={dialog.activeTab === tab.id}
                onClick={() => {
                  useDialogs.setState((state) => ({
                    dialogs: state.dialogs.map((item) =>
                      item.name === dialog.name ? { ...item, activeTab: tab.id } : item
                    ),
                  }));
                  fire("sclick", tab.id);
                }}
              >
                {tab.label}
              </button>
            ))}
          </div>
        )}
        <div className="dialog-body">
          {dialog.controls.map((c) => {
            if (["button", "tab", "menu", "item"].includes(c.kind) || !c.visible) return null;
            if (c.tab && c.tab !== dialog.activeTab) return null;
            const val = dialog.values[c.id] ?? "";
            const opts = dialog.options[c.id] ?? c.options;
            const disabled = !c.enabled;
            switch (c.kind) {
              case "text":
                return <div key={c.id} className="dlg-text">{c.label}</div>;
              case "edit":
                return (
                  <input
                    key={c.id}
                    className="dlg-edit"
                    type={c.styles.includes("pass") ? "password" : "text"}
                    readOnly={c.styles.includes("read")}
                    disabled={disabled}
                    autoFocus={dialog.focus === c.id}
                    value={val}
                    onChange={(e) => change(c.id, e.target.value)}
                  />
                );
              case "editbox":
                return (
                  <textarea
                    key={c.id}
                    className="dlg-editbox"
                    readOnly={c.styles.includes("read")}
                    disabled={disabled}
                    autoFocus={dialog.focus === c.id}
                    value={val}
                    onChange={(e) => change(c.id, e.target.value)}
                  />
                );
              case "check":
                return (
                  <label key={c.id} className="dlg-check">
                    <input
                      type="checkbox"
                      disabled={disabled}
                      checked={val === "1" || val === "2"}
                      ref={(element) => { if (element) element.indeterminate = val === "2"; }}
                      onChange={(e) => change(c.id, e.target.checked ? "1" : "0", "sclick")}
                    />
                    {c.label}
                  </label>
                );
              case "radio":
                return (
                  <label key={c.id} className="dlg-check">
                    <input
                      type="radio"
                      name={`${dialog.name}-radio-${c.tab || "main"}`}
                      disabled={disabled}
                      checked={val === "1"}
                      onChange={() => selectRadio(c)}
                    />
                    {c.label}
                  </label>
                );
              case "box":
                return <fieldset key={c.id} disabled={disabled}><legend>{c.label}</legend></fieldset>;
              case "link":
                return <a key={c.id} href={c.label} onClick={(event) => { event.preventDefault(); fire("sclick", c.id); }}>{c.label}</a>;
              case "icon":
                return <DialogIcon key={c.id} filename={c.label} label={c.id} />;
              case "scroll": {
                const range = c.styles.findIndex((style) => style === "range");
                return (
                  <input
                    key={c.id}
                    type="range"
                    disabled={disabled}
                    autoFocus={dialog.focus === c.id}
                    min={range >= 0 ? c.styles[range + 1] : "0"}
                    max={range >= 0 ? c.styles[range + 2] : "100"}
                    value={val}
                    onChange={(event) => change(c.id, event.target.value, "scroll")}
                  />
                );
              }
              case "combo":
                if (c.styles.includes("edit")) {
                  return (
                    <span key={c.id}>
                      <input
                        className="dlg-edit"
                        list={`${dialog.name}-${c.id}-options`}
                        disabled={disabled}
                        value={val}
                        onChange={(event) => change(c.id, event.target.value)}
                      />
                      <datalist id={`${dialog.name}-${c.id}-options`}>
                        {opts.map((option) => <option key={option} value={option} />)}
                      </datalist>
                    </span>
                  );
                }
                return (
                  <select
                    key={c.id}
                    className="dlg-combo"
                    disabled={disabled}
                    autoFocus={dialog.focus === c.id}
                    value={val}
                    onChange={(e) => change(c.id, e.target.value, "sclick")}
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
                    disabled={disabled}
                    multiple={c.styles.includes("multsel") || c.styles.includes("extsel")}
                    size={Math.min(Math.max(opts.length, 2), 8)}
                    value={c.styles.includes("multsel") || c.styles.includes("extsel") ? val.split("\n").filter(Boolean) : val}
                    onChange={(e) => change(
                      c.id,
                      Array.from(e.target.selectedOptions).map((option) => option.value).join("\n"),
                      "sclick"
                    )}
                    onDoubleClick={() => fire("dclick", c.id)}
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
            .filter((c) => c.visible && (!c.tab || c.tab === dialog.activeTab))
            .map((b) => (
              <button
                key={b.id}
                className={b.default ? "primary" : b.cancel ? "ghost" : ""}
                disabled={!b.enabled}
                autoFocus={dialog.focus === b.id}
                onClick={() => onButton(b.id, b.cancel, b.ok)}
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
