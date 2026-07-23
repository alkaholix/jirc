import { api } from "../lib/api";
import { useStore } from "../state/store";
import { useToolbar } from "../state/toolbar";

const isImage = (icon: string) =>
  /^(?:https?:|data:image\/)/i.test(icon);

export function ScriptToolbar() {
  const buttons = useToolbar((state) => state.buttons);
  const active = useStore((state) => (state.active ? state.buffers[state.active] : undefined));
  const servers = useStore((state) => state.servers);

  if (buttons.length === 0) return null;

  return (
    <div className="script-toolbar" role="toolbar" aria-label="Script toolbar">
      {buttons.map((button) => {
        const serverId = active?.serverId ?? button.serverId;
        const server = servers[serverId];
        return (
          <button
            key={button.name.toLowerCase()}
            className="script-toolbar-button"
            title={button.tooltip || button.name}
            onClick={() =>
              api
                .scriptRunPopup(
                  serverId,
                  active?.name ?? "",
                  server?.nick ?? "",
                  server?.name ?? "",
                  button.command,
                  [button.name],
                  undefined,
                  button.source
                )
                .catch(() => {})
            }
          >
            {isImage(button.icon) ? <img src={button.icon} alt="" /> : button.icon || button.name}
          </button>
        );
      })}
    </div>
  );
}
