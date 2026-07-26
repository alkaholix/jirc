import { relaunch } from "@tauri-apps/plugin-process";
import { check, type DownloadEvent } from "@tauri-apps/plugin-updater";

export type UpdateStatus =
  | { state: "idle" }
  | { state: "checking" }
  | { state: "current" }
  | { state: "available"; version: string; notes?: string }
  | { state: "downloading"; version: string; percent?: number }
  | { state: "error"; message: string };

let pendingUpdate: Awaited<ReturnType<typeof check>> = null;
let startupCheck: Promise<UpdateStatus> | null = null;

export async function checkForUpdate(): Promise<UpdateStatus> {
  pendingUpdate?.close();
  pendingUpdate = null;
  try {
    pendingUpdate = await check({ timeout: 15_000 });
    if (!pendingUpdate) return { state: "current" };
    return {
      state: "available",
      version: pendingUpdate.version,
      notes: pendingUpdate.body ?? undefined,
    };
  } catch (error) {
    return {
      state: "error",
      message: error instanceof Error ? error.message : String(error),
    };
  }
}

/** Checks once per app launch so remounts and extra windows cannot duplicate it. */
export function checkForUpdateOnStartup(): Promise<UpdateStatus> {
  startupCheck ??= checkForUpdate();
  return startupCheck;
}

export function updateNotificationBody(status: UpdateStatus): string | null {
  if (status.state !== "available") return null;
  return `Version ${status.version} is ready. Open Settings → Behaviour to review and install it.`;
}

export async function installUpdate(
  report: (status: UpdateStatus) => void
): Promise<void> {
  if (!pendingUpdate) {
    report({ state: "error", message: "Check for an update first." });
    return;
  }
  const update = pendingUpdate;
  let downloaded = 0;
  let total: number | undefined;
  report({ state: "downloading", version: update.version });
  try {
    await update.downloadAndInstall((event: DownloadEvent) => {
      if (event.event === "Started") total = event.data.contentLength;
      if (event.event === "Progress") downloaded += event.data.chunkLength;
      report({
        state: "downloading",
        version: update.version,
        percent: total ? Math.min(100, Math.round((downloaded / total) * 100)) : undefined,
      });
    });
    await relaunch();
  } catch (error) {
    report({
      state: "error",
      message: error instanceof Error ? error.message : String(error),
    });
  }
}
