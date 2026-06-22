// Nick colouring shared by the message area and the nick list: every nick uses
// the standard text colour, except the current user's own nick, which uses the
// accent (the blue used on the switchbar's active-tab border).

/** The default colour for your own nick (matches the accent blue). */
export const SELF_COLOR = "#7aa2f7";

/** Returns `selfColor` for your own nick, otherwise undefined so the nick
 *  inherits the standard text colour. */
export function nickColor(
  _nick: string,
  self = false,
  selfColor: string = SELF_COLOR
): string | undefined {
  return self ? selfColor : undefined;
}
