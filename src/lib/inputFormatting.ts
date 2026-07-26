export const IRC_FORMAT = {
  bold: "\x02",
  color: "\x03",
  reset: "\x0f",
  italic: "\x1d",
  underline: "\x1f",
} as const;

export interface TextEdit {
  value: string;
  selectionStart: number;
  selectionEnd: number;
}

export function insertControl(
  value: string,
  selectionStart: number,
  selectionEnd: number,
  control: string,
  close = control
): TextEdit {
  const selected = value.slice(selectionStart, selectionEnd);
  const suffix = selected ? close : "";
  return {
    value:
      value.slice(0, selectionStart) +
      control +
      selected +
      suffix +
      value.slice(selectionEnd),
    selectionStart: selectionStart + control.length,
    selectionEnd: selectionStart + control.length + selected.length,
  };
}

export function colorControl(foreground: number, background?: number): string {
  const fg = Math.max(0, Math.min(99, Math.trunc(foreground)))
    .toString()
    .padStart(2, "0");
  if (background === undefined) return `${IRC_FORMAT.color}${fg}`;
  const bg = Math.max(0, Math.min(99, Math.trunc(background)))
    .toString()
    .padStart(2, "0");
  return `${IRC_FORMAT.color}${fg},${bg}`;
}

export function applyPersistentColor(
  text: string,
  active: boolean,
  foreground: number,
  background?: number
): string {
  if (!active || text.startsWith("/")) return text;
  return `${colorControl(foreground, background)}${text}${IRC_FORMAT.reset}`;
}
