import type { HotlinkOutcome } from "./api";

/** Which popup menu a right-click should open, once `on HOTLINK` has had its say. */
export interface HotlinkMenuPlan {
  /** Popup contexts to fetch, in display order. Empty means open no menu. */
  contexts: string[];
  /** The clicked word, which a custom menu reads as `$1`. Empty otherwise. */
  word: string;
  /** The `/hotlink -m` menu name, or empty when the window's own menu is used. */
  custom: string;
}

/**
 * mIRC lets an `on HOTLINK` handler decide the menu for the word under the
 * pointer: `/halt` suppresses it, `/hotlink -m @name` replaces it, and `-d`
 * asks for the window's normal popups as well.
 *
 * `-d` on its own has no custom menu to add to, so it is the window's own menu
 * — which is what a click with no handler gets anyway.
 */
export function planHotlinkMenu(
  outcome: HotlinkOutcome | null,
  windowContext: string,
  word: string
): HotlinkMenuPlan {
  if (outcome?.halted) {
    return { contexts: [], word: "", custom: "" };
  }
  const custom = outcome?.menu ?? "";
  if (!custom) {
    return { contexts: [windowContext], word: "", custom: "" };
  }
  return {
    contexts: outcome?.defaults ? [custom, windowContext] : [custom],
    word,
    custom,
  };
}
