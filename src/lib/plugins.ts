import type { Buffer } from "../state/store";
import { useStore } from "../state/store";
import { notify } from "./notify";
import { api, type PluginDispatch } from "./api";

/** Applies the deliberately small set of capabilities returned by sandboxed
 * plugins. A plugin never receives a Tauri handle or arbitrary invoke access. */
export async function applyPluginDispatch(
  result: PluginDispatch,
  buffer?: Buffer,
  runCommand?: (command: string) => Promise<void>
): Promise<void> {
  const store = useStore.getState();
  for (const action of result.actions) {
    if (action.type === "notify") {
      await notify(action.title, action.text);
    } else if (action.type === "echo") {
      const serverId = buffer?.serverId ?? Object.keys(store.servers)[0];
      if (!serverId) continue;
      const target = action.target || buffer?.name || "(status)";
      const kind = target === "(status)" ? "status" : target.startsWith("#") ? "channel" : "query";
      store.appendLine(serverId, target, kind, { kind: "system", text: action.text });
    } else if (action.type === "command" && runCommand) {
      await runCommand(action.command);
    }
  }
  if (result.errors.length && buffer) {
    for (const error of result.errors) {
      store.appendLine(buffer.serverId, "(status)", "status", {
        kind: "error",
        text: `[plugin] ${error}`,
      });
    }
  }
}

export async function dispatchPluginEvent(
  event: string,
  payload: unknown,
  buffer?: Buffer,
  runCommand?: (command: string) => Promise<void>
) {
  const result = await api.pluginDispatch(event, payload);
  await applyPluginDispatch(result, buffer, runCommand);
  return result.handled;
}
