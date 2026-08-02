import { api } from "../lib/api";
import { useState } from "react";
import { ScriptPanelItem, usePanels } from "../state/panels";
import { useStore } from "../state/store";

export function ScriptPanels() {
  const panels = usePanels((state) => state.panels);
  const active = useStore((state) => (state.active ? state.buffers[state.active] : undefined));
  const servers = useStore((state) => state.servers);

  if (panels.length === 0) return null;

  const run = (panelServerId: string, item: ScriptPanelItem, value = item.value) => {
    const serverId = active?.serverId ?? panelServerId;
    const server = servers[serverId];
    return api.scriptRunPopup(serverId, active?.name ?? "", server?.nick ?? "", server?.name ?? "", item.command, [item.id, value], undefined, item.source).catch(() => {});
  };

  return (
    <aside className="script-panels" aria-label="Script panels">
      {panels.map((panel) => (
        <section className="script-panel" key={panel.name.toLowerCase()}>
          <h3>{panel.title}</h3>
          <div className="script-panel-body">
            {panel.items.map((item) =>
              item.kind === "separator" ? (
                <hr className="script-panel-separator" key={item.id.toLowerCase()} />
              ) : item.kind === "text" ? (
                <div className="script-panel-text" key={item.id.toLowerCase()}>
                  {item.value}
                </div>
              ) : item.kind === "progress" ? (
                <label className="script-panel-control" key={item.id.toLowerCase()}>
                  {item.label && <span>{item.label}</span>}
                  <progress max={100} value={Math.max(0, Math.min(100, Number(item.value) || 0))} />
                </label>
              ) : item.kind === "input" ? (
                <PanelInput key={item.id.toLowerCase()} item={item} run={(value) => run(panel.serverId, item, value)} />
              ) : item.kind === "checkbox" ? (
                <label className="script-panel-control" key={item.id.toLowerCase()}>
                  <input type="checkbox" defaultChecked={item.value !== "0"} onChange={(event) => void run(panel.serverId, item, event.target.checked ? "1" : "0")} />
                  {item.label || item.id}
                </label>
              ) : (
                <button
                  key={item.id.toLowerCase()}
                  onClick={() => {
                    void run(panel.serverId, item);
                  }}
                >
                  {item.label || item.id}
                </button>
              )
            )}
          </div>
        </section>
      ))}
    </aside>
  );
}

function PanelInput({ item, run }: { item: ScriptPanelItem; run: (value: string) => void }) {
  const [value, setValue] = useState(item.value);
  return <label className="script-panel-control">
    {item.label && <span>{item.label}</span>}
    <input value={value} onChange={(event) => setValue(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter") run(value); }} />
  </label>;
}
