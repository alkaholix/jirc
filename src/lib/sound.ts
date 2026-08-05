import { convertFileSrc } from "@tauri-apps/api/core";
import { useSettings } from "../state/settings";

export type AlertSound = "mention" | "private" | "invite" | "online";

let player: HTMLAudioElement | null = null;

export function isQuietTime(
  now: Date,
  enabled: boolean,
  from: string,
  to: string
): boolean {
  if (!enabled || !/^\d\d:\d\d$/.test(from) || !/^\d\d:\d\d$/.test(to)) return false;
  const current = now.getHours() * 60 + now.getMinutes();
  const [fromHour, fromMinute] = from.split(":").map(Number);
  const [toHour, toMinute] = to.split(":").map(Number);
  const start = fromHour * 60 + fromMinute;
  const end = toHour * 60 + toMinute;
  return start <= end
    ? current >= start && current < end
    : current >= start || current < end;
}

function tone(volume: number, frequency = 660): void {
  if (typeof window === "undefined" || !window.AudioContext) return;
  const AudioContextClass = window.AudioContext;
  const context = new AudioContextClass();
  const oscillator = context.createOscillator();
  const gain = context.createGain();
  oscillator.frequency.value = frequency;
  gain.gain.setValueAtTime(Math.max(0, Math.min(1, volume)), context.currentTime);
  gain.gain.exponentialRampToValueAtTime(0.001, context.currentTime + 0.18);
  oscillator.connect(gain).connect(context.destination);
  oscillator.start();
  oscillator.stop(context.currentTime + 0.18);
  oscillator.addEventListener("ended", () => context.close().catch(() => {}));
}

export async function playFile(path: string, volume?: number, ended?: () => void): Promise<void> {
  if (!path) return;
  player?.pause();
  player = new Audio(convertFileSrc(path));
  player.volume = Math.max(0, Math.min(1, volume ?? useSettings.getState().soundVolume));
  if (ended) player.addEventListener("ended", ended, { once: true });
  await player.play();
}

export function controlAudio(operation: string, path = "", ended?: () => void): void {
  if (operation === "stop") {
    player?.pause();
    if (player) player.currentTime = 0;
  } else if (operation === "pause") {
    player?.pause();
  } else if (operation === "resume") {
    player?.play().catch(() => {});
  } else if (operation === "play") {
    playFile(path, undefined, ended).catch(() => {});
  } else if (operation === "beep") {
    tone(useSettings.getState().soundVolume);
    ended?.();
  }
}

export function playAlertSound(kind: AlertSound, preview = false): void {
  const settings = useSettings.getState();
  if (
    !preview &&
    (!settings.soundEnabled ||
      isQuietTime(
        new Date(),
        settings.quietHoursEnabled,
        settings.quietHoursFrom,
        settings.quietHoursTo
      ))
  ) {
    return;
  }
  const path = settings[`${kind}Sound` as const];
  if (path) playFile(path, settings.soundVolume).catch(() => tone(settings.soundVolume));
  else tone(settings.soundVolume, kind === "private" ? 760 : kind === "invite" ? 880 : 660);
}
