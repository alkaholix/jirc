import { api } from "../lib/api";
import { usePanels } from "../state/panels";
import { useStore } from "../state/store";

export function ScriptPanels() {
  const panels = usePanels((state) => state.panels);
  const active = useStore((state) => (state.active ? state.buffers[state.active] : undefined));
  const servers = useStore((state) => state.servers);

  if (panels.length === 0) return null;

  return (
    <aside className="script-panels" aria-label="Script panels">
      {panels.map((panel) => (
        <section className="script-panel" key={panel.name.toLowerCase()}>
          <h3>{panel.title}</h3>
          <div className="script-panel-body">
            {panel.items.map((item) =>
              item.kind === "text" ? (
                <div className="script-panel-text" key={item.id.toLowerCase()}>
                  {item.value}
                </div>
              ) : (
                <button
                  key={item.id.toLowerCase()}
                  onClick={() => {
                    const serverId = active?.serverId ?? panel.serverId;
                    const server = servers[serverId];
                    api
                      .scriptRunPopup(
                        serverId,
                        active?.name ?? "",
                        server?.nick ?? "",
                        server?.name ?? "",
                        item.command,
                        [item.id],
                        undefined,
                        item.source
                      )
                      .catch(() => {});
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
