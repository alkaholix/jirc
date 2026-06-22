import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";

let granted: boolean | null = null;

async function ensurePermission(): Promise<boolean> {
  if (granted !== null) return granted;
  granted = await isPermissionGranted();
  if (!granted) {
    granted = (await requestPermission()) === "granted";
  }
  return granted;
}

/** Shows a desktop notification (best effort; ignored if not permitted). */
export async function notify(title: string, body: string): Promise<void> {
  try {
    if (await ensurePermission()) {
      sendNotification({ title, body });
    }
  } catch {
    /* notifications unavailable */
  }
}
