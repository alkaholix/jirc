<div align="center">

# 📜 jIRC — Changelog

**A modern, cross-platform IRC client with a built-in mIRC-style scripting engine.**

Speaks standard IRC (RFC 1459/2812 + IRCv3) and **IRCX** · runs **mSL** scripts natively · MIT licensed

Versions use CalVer (`YY.M.D`) — newest first.

</div>

---

## 🗂️ 26.8.28 — The menu bar is back, as a choice

- **Settings → Appearance → Menu bar** switches the top bar between **Icons**
  and **Menus**. Icons stay the default; nobody's bar changes unless they ask.
- The text row is **File · Settings · Scripts · Tools · Channels · Help**. Two
  labels differ from the old bar, both because the originals did not say what
  they did: *View* opened the Settings dialog and is now **Settings**, and
  **Channels** (auto-join) was missing from the text bar entirely even though
  the icon row had it. Every entry opens exactly what its icon does, so the two
  styles differ in appearance and nothing else.
- **Commands** still appears only when a script defines menubar items, and
  shows as `≡` in icon mode so it matches whichever style the bar is in.

### Scripting
- **`$mode(N)` now exists.** It reports the Nth nick a mode change affected —
  `$mode(0)` for the count — with all ten properties: `.owner` `.deowner` `.op`
  `.deop` `.help` `.dehelp` `.voice` `.devoice` `.ban` `.unban`. It reads the
  event's own mode string, and ISUPPORT decides which letters carry an argument,
  so on a network where `+q` is the quiet list you get the mask rather than a
  nick.
- **Fixed: `/join` and `/msg` ignored their switches.** `/join -i` sent a literal
  `JOIN -i` to the server and `/msg -s bob hi` messaged a target called `-s`.
  Both now parse them: `-i` joins the channel you were last invited to, and the
  window-state switches — which ask for an MDI layout jIRC has no equivalent of
  — are consumed rather than passed to the network.

---

## 🧮 26.8.27 — `$input` options, and evaluation brackets that count

### `$input` finally reads its options
The second argument — mIRC's option field — was ignored outright, so
`$input(msg,y)` showed a text box instead of Yes/No and the identifiers that
report which button was pressed had nothing to report.

- **Buttons** `o` `y` `n` `r`, **fields** `e` (text) `p` (password) `m`
  (dropdown), **icons** `t c i q w h`, and **`kN`** for an N-second timeout with
  a visible countdown. The `s` switch correctly shifts the later arguments.
- **Both return conventions.** With buttons only you get `$true`/`$false`, or the
  named values with `v`. With a field you get the text, and `f` turns a
  dismissal into a name rather than nothing. `$timeout` needs `v`, plus `f` when
  a field is in play.
- **`$yes` `$no` `$ok` `$cancel` `$retry` `$timeout`** now exist, so
  `if ($input(Save first?,nv) == $cancel)` can tell Cancel from No.
- The older numeric option form works too: `$input(hi,5)` is `$input(hi,eo)`.

### Evaluation brackets evaluate the right number of times
`[ ]` already reordered evaluation correctly, but every group was evaluated
**once too often**: the bracket pass runs before the token pass, which then
evaluated each group's *result* as well. `[ $!me ]` printed your nick where mIRC
prints `$me`.

- One pair is one evaluation; each further pair adds another, so
  `[ [ $!me ] ]` reaches the nick.
- **A space stops the counting**, as in mIRC: if the contents contain a space
  that no `$+` closes, every enclosing pair beyond the first is ignored, and
  `[ [ a $!me ] ]` gives `a $me`.
- The `$+` join form is untouched — `% [ $+ [ %k ] ]` still builds a name and
  dereferences it, which is why that extra evaluation existed at all.

### Help
Two new sections: **Asking the user — $input**, covering the options and what
each returns, and **Evaluation brackets**, including the space rule that catches
people out.

---

## ✨ 26.8.26 — Tidier tree, and a proper welcome

- **The welcome screen shows the app icon and the version.** The `#` mark sits
  beside the wordmark with the version underneath, so which build you are running
  is visible without opening About.
- **Removed the "jIRC" bar above the tree.** It was left over from when that row
  held the toolbar icons; once those moved to the top bar in 26.8.24 it was a
  full-width strip containing nothing but the app's own name. The tree now starts
  at the top, matching the switchbar layout, which never had one.

---

## 🔗 26.8.25 — Every input-bar command now works from scripts

Typed input falls through to the script engine, but a script never reached the
input bar's own handlers. A command implemented only there worked when typed
and, when scripted, was **sent to the server as a verb** — `/ignore bob` put a
literal `IGNORE bob` on the wire, silently doing nothing locally while telling
the network what you meant. All 53 input-bar commands were tested through the
engine; ten were broken:

- **`/op` `/deop` `/voice` `/devoice`** sent `OP bob` to the server instead of
  setting the mode. They now emit `MODE #chan +o bob`, accept an optional
  leading channel (`/op #other bob`), take several nicks at once batched to the
  server's `MODES` limit, and stay silent where the server has no such mode —
  so nothing is sent on a network where `+q` means *quiet* rather than owner.
- **`/ignore` `/unignore` `/notify` `/urls` `/url` `/wc`** now reach the client
  instead of the server.
- **`/query <nick>`** opens the query window. Previously a bare `/query` from a
  script did nothing at all.
- **`/k` `/b` `/w` `/wi`** work as the short forms they are.

The shared bodies live in one module used by both paths, and a test pins the
whole surface, so a future command added on one side only fails the build
rather than leaking silently.

### Help
- **New "Access lists" section** covering `/aop`, `/avoice`, `/protect` and
  `/aowner` — the forms they take, that switching a list off does not empty it,
  and why jIRC declines to act when you could not set the mode yourself.
- **New "Channel and list tests" section** for the `is…` operators, spelling out
  the difference between asking the *server* about live status (`isowner`) and
  asking your *saved lists* (`isaowner`) — the mix-up behind an earlier bug.
- Identifier reference gained the access-list identifiers; typing notifications,
  MONITOR and the newer commands are documented.

### Housekeeping
- `/halfop`, `/owner` and `/admin` are **removed**. They are in neither mIRC nor
  jIRC's input bar and were added only because `/op` existed. `goodidea.md` now
  records anything jIRC has that mIRC does not, for review rather than quiet
  accumulation.
- Fixed a test that asserted `$adate` and `$date` always differ. `$adate` is
  MM/DD/YYYY and `$date` DD/MM/YYYY, so they match on the twelve days a year
  where day equals month — it failed on 08/08.

---

## 🧭 26.8.24 — One icon bar instead of text menus

- **The File / View / Scripts / Commands / Tools / Help menus are gone.** The
  top bar now holds the icon actions — About, Scripts, Address book, Auto-join,
  Settings, Add connection. Every one of those menus already duplicated an icon,
  so nothing was lost with them.
- Those icons previously appeared **twice**, once in the tree sidebar and once
  in the switchbar. They now exist once, in the top bar, so the sidebar header
  is just the jIRC label and the switchbar is just tabs.
- Script-defined menubar menus (`menu menubar { … }`) still work, but the button
  is **hidden until a script actually defines items** — previously it sat there
  as a menu that did nothing when clicked — and appears as an `≡` icon in
  keeping with the rest of the bar.

---

## 👑 26.8.23 — IRCX uses its own prefixes

- **Fixed: channel owners showed a `~` instead of IRCX's `.`.** On a server that
  sends no `PREFIX` token, jIRC fell back to the standard `~&@%+` table. IRCX's
  set is `.@+` — owner is `.`, there is no halfop or admin rank, and `%`/`&` are
  *channel-name* characters there (`%#room`), so listing them as member prefixes
  was wrong on both counts.
- IRCX connections now announce their prefix table at connect time rather than
  waiting for a `005` that older servers never send. Without it the nicklist
  kept the standard table and sorted owners **below** ordinary users, unranked
  and uncoloured, even once the backend had them right.
- A server that does state its own `PREFIX` is still believed exactly as sent,
  IRCX or not.

---

## 🛠️ 26.8.22 — Channel modes on pre-ISUPPORT IRCX servers

- **Fixed: channel modes lost their parameters on old IRCX servers**, showing
  as a bare `Snue sets mode: +q` with no nick. The original Exchange 5.5 Chat
  Service predates `RPL_ISUPPORT` and advertises nothing, so jIRC never learned
  that `%` starts a channel name — `MODE %#chan +q nick` did not look like a
  channel mode and took the *user*-mode path, which renders mode letters
  without their arguments.
  - This affected **every** parameterised mode (`+o`, `+v`, `+b` too), not just
    `+q`; it also meant channel member status was not updated from MODE, and a
    channel mode was being written into your own user-mode string, corrupting
    `$usermode`.
  - IRCX connections now recognise the unambiguous `%#`/`%&` channel forms even
    with no `CHANTYPES` token, matching the fallback channel-target resolution
    has always had. Gated on IRCX: `%` is a STATUSMSG prefix elsewhere, where
    `%#chan` addresses the halfops of `#chan` rather than naming a channel.
- IRCX connections whose server *does* send `PREFIX` but omits `q` now still
  treat `+q` as taking a nick, and record owner status rather than only
  displaying it. Non-IRCX networks are untouched — on Charybdis-family servers
  `+q` is the quiet list and continues to take a mask.

---

## ⌨️ 26.8.21 — Typing indicators, MONITOR, and an owners list

### Typing notifications
- **"Bob is typing…" above the composer**, from the IRCv3 `+typing` tag. Scales
  to "Bob and Sue", then "4 people are typing…". The row is absent when nobody
  is typing, so the input never jumps.
- A paused typist still shows — they are composing, just not that instant. A
  message from them clears it, as does parting or quitting, and a stale
  notification expires after 6 seconds so a client that drops mid-compose does
  not leave the indicator stuck.
- Nothing is sent while you are typing a `/command`, and outgoing notifications
  are throttled to one every three seconds.
- Two toggles in **Settings → General**: show others' typing, and let others
  see yours.

### Notify list
- **MONITOR replaces ISON polling** wherever the server advertises it, so the
  watch list updates the moment someone connects instead of up to 30 seconds
  later. Servers without MONITOR keep polling; a full monitor list falls back
  to polling for that server.

### Owners list (jIRC extension — mIRC has no equivalent)
- **`/aowner`**, `$aowner` and the `isaowner` operator, matching `/aop` in every
  respect, plus an **Auto-owner** section in Settings → Users. Sets `+q` on
  join, and outranks auto-op.
- Inert where the server does not advertise `q` as a member prefix — on
  Charybdis-family networks `+q` is the quiet list, so auto-owner there would
  have quieted the person it was meant to promote.
- **Fixed: the list operators always answered "no".** `isaop`, `isavoice`,
  `isprotect`, `isnotify` and `isignore` were hardcoded to false, which was
  right when jIRC kept no such lists — but `/aop` and friends landed later and
  the operators were never updated, so the list a command wrote and the operator
  that read it disagreed. They now consult the real list. Membership only:
  `/aop off` stops the automatic op without emptying the list.
- `isowner` is unchanged — it remains a live channel-state test, like `isop`.

### Other IRCv3
- **standard-replies**: `FAIL`, `WARN` and `NOTE` are shown with the command
  they relate to and their machine-readable code.
- **STS**: a policy advertised over plaintext reconnects to the server's TLS
  port immediately. The upgrade is not written back to the saved profile.
- Also requested: `utf8only` and `extended-monitor`.
- `draft/multiline` and `draft/pre-away` are deliberately **not** requested —
  acknowledging a capability promises the server how the client will behave,
  and jIRC does not yet reassemble multiline batches.

---

## 🚪 26.8.20 — CTCP no longer opens a window

- Fixed `/ctcp <nick> <request>` opening a query window named after the target.
  The outgoing message was echoed locally like ordinary conversation, so the
  request appeared as a message from you in a new window. mIRC shows
  `-> [nick] VERSION` in the active window and opens nothing, which is now what
  jIRC does. The same applies to `/ctcpreply`.
- Actions are unaffected: `/me` is conversation and still echoes into the buffer
  it was sent to.

---

## 🪟 26.8.19 — Notices and CTCP replies go where mIRC puts them

- Notices no longer open a window. A notice carrying a sender was routed into a
  new query buffer named after them, so NickServ, ChanServ and every other
  service opened a tab of its own; only senderless server notices reached the
  console. Services now report to the server console, as in mIRC. A notice from
  someone you already have a query open with still appears there, so one
  arriving mid-conversation stays with the rest of it.
- CTCP replies are echoed in the active window rather than the server console,
  matching mIRC. This covers replies carried by an IRCX whisper, which 26.8.18
  showed in the whisper's channel.

---

## 💬 26.8.18 — CTCP over IRCX whispers

- Fixed CTCP payloads carried by an IRCX `WHISPER` being shown as ordinary
  whisper text. A reply to `/ctcp <nick> VERSION` on a network that answers by
  whisper appeared as "Snue whispers: VERSION mIRC v7.84"; it now reads as
  `[CTCP reply from Snue] mIRC v7.84`, in the channel the whisper was scoped to.
  An action sent by whisper shows its text instead of the control characters
  around it.
- Fixed whispers never reaching the scripting engine. Nothing forwarded them,
  so on IRCX networks neither `on TEXT` nor `on CTCPREPLY` fired for a whisper,
  silently, however the script was written. They are now dispatched like any
  other message.
- A whispered CTCP is treated as a reply rather than a request. IRC marks the
  difference with `PRIVMSG` against `NOTICE` and `WHISPER` has no equivalent, so
  the safe reading is the one that cannot make two clients auto-reply to each
  other indefinitely. The consequence is that a genuine CTCP request sent by
  whisper receives no automatic answer.

---

## 🧠 26.8.17 — Advanced default popups, and a script editor fix

- Fixed the script editor showing a stale cached draft instead of the file on
  disk. A draft took priority unconditionally, so once one existed for a script
  it hid that file permanently — including defaults rewritten by an upgrade —
  and nothing in the interface could clear it.
- A draft now records the text it was taken from, and is discarded when the file
  changes underneath it. Drafts saved before this release cannot be checked that
  way and are cleared once, on first run.
- Added a **Discard changes** button, so unsaved edits can be thrown away and
  the file reloaded. Previously a draft could only be cleared by saving over it
  or deleting the script.
- Rebuilt the default popup menus so they demonstrate what the scripting engine
  can do rather than listing a few commands:
  - Channel modes tick according to the channel's live mode string and toggle
    the opposite way when already set.
  - The channel menu lists the current ban list, built at open time, and
    clicking an entry lifts that ban.
  - The nick-list menu offers Give or Take ops depending on what the person
    already holds, previews all ten `$mask` types against their real address
    before you ban, and lists the channels you share with them.
  - Channel lists show each channel's user count, the status menu reports
    uptime and away duration, and the multi-selection group acts on every
    selected nick.

---

## 🖱️ 26.8.16 — Default popup menus, corrected

Supersedes 26.8.15, which shipped the new popup menus to the wrong file. Update
to this release rather than 26.8.15.

- Fixed the default popup menus being written to `popups.mrc` instead of the
  per-context files the script editor uses. The editor's Popups section edits
  one file per context — Server/status, Channel, Nick list, Query, and Custom
  window — so the menus added in 26.8.15 never appeared on those tabs, and any
  menu they defined was duplicated against the per-context file for the same
  context.
- The defaults are now seeded as `popups-status`, `popups-channel`,
  `popups-nicklist`, and `popups-query`, matching the editor tabs exactly.
  `popups.mrc` returns to being the empty combined file it is described as, for
  imported or legacy menu blocks.
- Added a test asserting the seeded files and the editor's fallback templates
  stay identical, so the two copies cannot drift apart.

If you installed 26.8.15, delete `popups.mrc` (or empty it) and delete the
`popups-*.mrc` files you have not customised, then use Add examples in the
script editor to write the corrected defaults.

---

## 🖱️ 26.8.15 — Default popup menus

- Replaced the seeded `popups.mrc` example, which defined a single nick-list
  menu, with default right-click menus for the channel, nick list, query, and
  status windows.
- The defaults demonstrate the popup engine rather than just listing commands:
  operator actions grey themselves out when you do not hold ops, the away item
  carries a live check mark, "Jump to" builds one entry per joined channel with
  `$submenu`, and the nick-list group acts on a multiple selection through
  `$snicks`.
- The shipped file is ordinary mSL kept alongside the source and included at
  build time, so it stays readable and is covered by the test that verifies the
  menus it produces.
- Existing installations keep their current `popups.mrc`; jIRC only writes
  example scripts that do not already exist. Delete or rename yours and use
  Add examples to pick up the new defaults.

---

## 🧩 26.8.14 — mSL parity completion

- Added the client-side commands that previously fell through to the IRC server
  as invalid protocol and failed silently: `/leave`, `/action`, `/partall`,
  `/exit`, `/disconnect`, `/closemsg`, `/clearial`, `/vmsg`, `/vnotice`,
  `/wallchops`, `/wallvoices`, `/ctcps`, and `/colour`.
- `/vmsg`, `/vnotice`, `/wallchops`, and `/wallvoices` use a `@#channel` or
  `+#channel` STATUSMSG target when the server advertises the prefix, and
  address the matching members individually when it does not.
- Fixed the parser silently discarding event handlers written in mIRC's
  documented form. Events whose syntax is `ON <level>:EVENT:<commands>` have no
  matchtext or target field, but the parser read the command as a target, found
  an empty command, and dropped the handler without an error. This affected
  `on DNS`, `on START`, `on LOAD`, `on UNLOAD`, `on EXIT`, `on QUIT`, `on NICK`,
  `on USERMODE`, `on AGENT`, and the five playback events.
- Added `on MIDIEND`, `on MP3END`, and `on SONGEND`. `/splay` now selects the
  end event from the file extension and pairs each sound event with `on SONGEND`.
- Recognised `isaop`, `isavoice`, `isignore`, `isprotect`, `isnotify`, and
  `isquiet`. An unrecognised operator previously fell through to a truthiness
  test and read as always true, so `if (%address isaop)` granted access to
  everyone; these now test against an empty list and fail closed.
- Fixed `$color`, which returned an RGB value for both of its forms. `$color(N)`
  returns the RGB for colour index N, while `$color(<name>)` returns the index
  number. Added the `.dd` property and mIRC's partial name matching.
- Fixed `$isalias`, which ignored its property argument, and added `.fname` and
  `.ftype`.
- Added identifiers `$colour`, `$naddress`, `$iaddress`, `$rnick`, `$nvnick`,
  `$nopnick`, `$nhnick`, `$adate`, `$evalnext`, `$isnumber`, `$isutf`, `$lof`,
  `$freadex`, `$hfile`, `$hmatch`, `$hregex`, `$regbr`, `$banlist`, `$iql`,
  `$initopic`, `$factorial`, `$fibonacci`, `$fserv`, `$fupdate`, `$mp3dir`,
  `$wavedir`, `$inmp3`, `$vc`, `$nonstdmsg`, `$inmode`, `$inwho`,
  `$sslcertsha1`, and `$sslcertsha256`.
- Added the `$chan().banlist` and `$chan().inwho` properties, backed by
  listing-in-flight state that the `RPL_ENDOFBANLIST` and `RPL_ENDOFWHO`
  numerics now clear. They share a sentinel with `$inmode` and `$inwho` so
  mIRC's comparison idiom works.
- Added properties `$iptype().expand`/`.compress`, `$fopen().bom`, and
  `$sslhash().babble`, the last verified against the Bubble Babble
  specification's published test vectors.
- Added the `/fsend` and `/fupdate` client settings, and `$freadex` for reading
  a file from its pointer to the end.
- Removed exponential backtracking from wildcard matching, which could hang the
  client on a pattern such as `*a*a*a*b` against a long non-matching message.
- Added an example popup file (`popups.mrc`) covering the channel, nicklist,
  query, status, and menu bar contexts, with dynamic `$submenu` lists,
  `$style` states, nested menus, and multi-selection helpers.

---

## 🎨 26.8.13 — mSL accuracy and interface polish

- Fixed `$mask` and every identifier built on it. The mask type table was
  offset by one, so each type returned the mask for the type below it. This
  also corrected `/ban`, which produced `*!*user@host` instead of the intended
  `*!*@host` for the default type 2, and `$address`.
- Added `^` (exponent) and `//` (floor division) to `$calc`, which previously
  returned `$null` for any expression using them. `^` binds tighter than
  `* / // %` and evaluates left to right, so `$calc(4 ^ .5 ^ 3)` is `8`.
- `$calc` and `$abs` now round to six decimal places as mIRC does, so
  `$calc(1/3)` returns `0.333333` rather than full float precision.
- Recognised `isaop`, `isavoice`, `isignore`, `isprotect`, `isnotify`, and
  `isquiet`. Previously an unrecognised operator fell through to a truthiness
  test, so `if (%address isaop)` was always true; these now test against an
  empty list and return false.
- `<`, `>`, `<=`, and `>=` fall back to lexicographic comparison when either
  side is not numeric, so `if (apple < banana)` is true.
- Applied mIRC's null-token rule to every token identifier rather than only
  `$gettok` and `$numtok`. Consecutive, leading, and trailing delimiters no
  longer shift token positions or survive into the result of `$findtok`,
  `$deltok`, `$puttok`, `$remtok`, `$reptok`, `$addtok`, `$instok`, `$sorttok`,
  `$matchtok`, `$wildtok`, `$istok`, and their `cs` variants.
- Fixed `$deltok` with a negative index, which deleted the first token instead
  of counting back from the last, and added support for reversed ranges such as
  `-1--2`. `$gettok(list,0,C)` now returns the token count.
- `$time`, `$date`, and `$ctime` accept their arguments. `$time` and `$date`
  take `$asctime`'s format string, and `$ctime(text)` parses a date, including
  `d/m/y`, `yyyy-m-d`, month names, ordinals, weekday prefixes, and am/pm.
- Fixed `$round` so an omitted decimal count leaves the number unchanged and
  fractions are no longer padded with trailing zeros. `$base` converts
  fractions and honours its precision parameter.
- Added `on MIDIEND`, `on MP3END`, and `on SONGEND`. `/splay` selects the event
  from the file extension and pairs each sound event with `on SONGEND`.
- Fixed the parser dropping playback events written in mIRC's documented form.
  `on *:MP3END:<command>` takes no matchtext or target field; it previously
  parsed the command as a target and discarded the handler, which also affected
  `on WAVEEND` and `on PLAYEND`.
- `$regex` returns a negative result for an invalid pattern instead of `0`, so
  a bad expression is distinguishable from no match. `$sorttok`'s `a` switch
  sorts numeric tokens after non-numeric ones. `isnum` no longer accepts `inf`,
  `NaN`, or exponent forms, and `islower`/`isupper` allow non-alphabetic text.
  `$asc` reports the leading surrogate for characters above the BMP.
- Removed exponential backtracking from wildcard matching, which could hang on
  a pattern such as `*a*a*a*b` against a long non-matching message.
- Unified the accent colour on a brand ramp derived from the application icon.
  The light theme and the About mark previously used blues that did not appear
  in the logo, one of them noticeably darker and duller. Both themes now use
  the same accent, so the blue is identical throughout the application.
- Rebuilt the message colour picker. The Apply button no longer paints itself
  in the selected IRC colours and no longer needs a text shadow to stay
  readable; a separate swatch previews the choice, and a 16-cell palette grid
  replaces the two dropdowns, which could not show colours on Windows.
- Reorganised Settings into a grouped sidebar with search, replacing seven tabs
  that wrapped onto two rows. Options share one row layout, and the dialog no
  longer changes height between sections.

---

## 🔗 26.8.12 — Safe URL previews

- Added URL preview cards beneath channel, query, action, notice, and whisper
  messages, with user-selectable Compact, Rich card, and Image-first layouts in
  Settings → Appearance.
- Added Open Graph, Twitter Card, HTML-title, description, direct-image, and
  relative-image discovery with session caching and a three-link-per-message
  limit.
- Preview requests run through the Rust backend without cookies or scripts.
  DNS-pinned public-address validation, redirect revalidation, timeouts,
  download limits, content-type checks, and private/local network blocking
  protect users; preview images are returned as local data URLs.

---

## 🎭 26.8.11 — Complete portable event compatibility

- Added `on APPACTIVE`, `on CHAR`, `on HOTLINK`, `on LOGON`, `on SERV`,
  `on SERVERMODE`, `on SERVEROP`, and `on NOSOUND` with their live application,
  custom-window, rendered-message, registration, Fserve, IRC mode, and CTCP
  dispatch paths.
- Added `$hotlink(...)`, `$hotline`, `$hotlinepos`, character identifiers,
  Fserve `$cd`, server-mode nick context, and missing-sound `$filename` state.
- Added parser and runtime regression coverage for the complete portable event
  tail. Obsolete Agent, voice-command, MIDI, and MP3 event families remain
  stable inactive compatibility surfaces.

---

## 🧩 26.8.10 — Complete command compatibility pass

- Completed the remaining actionable mSL command surface with runtime, user
  list, custom-window, DCC policy, audio and client-layout behavior.
- Added script control for auto-joining invites, outbound flood limits, local
  bind information, tracing, quiet mode, request policies, volume, clipboard,
  window attention, opacity and native minimize/restore behavior.
- Added `/rlevel`, `/ulist`, `/uwho`, `/cline`, `/playctrl`, `/reseterror` and
  compatibility-state commands with regression coverage.
- Legacy Microsoft Agent, voice-command and speech commands are accepted as
  stable inactive compatibility operations; identd remains demand-deferred for
  older networks, and DLL/COM/DDE commands remain permanent cross-platform
  non-goals.

## 🧩 26.8.9 — Complete identifier compatibility pass

- Completed the remaining cross-platform mSL identifier surface, including
  live connection, event, channel-list, window-history, DCC, client UI and
  local-system state.
- Added file selection, clipboard, disk, path, archive, compression, Argon2,
  hashing, wrapping, picture and address-book identifiers with sandboxed file
  access where applicable.
- Added real system uptime, local host/address discovery, mode-batch boundaries,
  IRC LINKS and channel ban/exception/invite list tracking.
- Preserved explicit inactive results for legacy speech/agent/voice-command
  facilities that jIRC does not provide, and retained COM/DDE/DLL identifiers as
  permanent cross-platform non-goals.

## 🧩 26.8.8 — Script UI state and compatibility

- Added stateful mSL `$tip`/`$tips` and `/tip`/`/tips` support with named desktop
  notifications, in-app tips, expiry, inspection, text updates, closing,
  connection/window association and double-click alias callbacks.
- Added a persistent application menubar with `/menubar` and `$menubar` state.
  Script-defined `menu menubar` entries appear under its Commands menu.
- Added `$menu`, `$menutype` and `$menucontext` during popup evaluation, including
  custom `@window` menu types.
- Added `$markasread` backed by live buffer unread/mention state.
- Added `$fromeditbox` provenance through aliases invoked from message inputs.
- Added IRCv3 `invite-notify` and `setname` negotiation; `SETNAME` now updates
  IAL real-name data and shared-channel notices.
- Added optional native operating-system script popup menus, including nested,
  checked, disabled and separator items, with automatic WebView fallback.
- Added a sandboxed cross-platform Luau plugin API with IRC event and command
  hooks, validated echo/command/notification capabilities, enable controls,
  resource limits, an example generator and a dedicated `plugins/` folder.

## 🧩 26.8.7 — Dockable panes

- Live connections now adopt the server's `005 NETWORK=` display name (including
  local bridge scripts such as `i7.mrc`) without changing the saved profile.
- Added mSL `$ignore`, `$highlight`, `$font`, and `$editbox` identifiers backed
  by live client state, plus `/findtext` integration with buffer search.
- Added draggable left/right docking and persistent ordering for the treebar,
  channel nick list and script-created panels.
- Added pointer-resizable pane edges with independent persisted widths.
- Added accessible side-switch buttons and an Appearance setting to reset the
  complete pane layout.
- Preserved the compact switchbar layout and independent detached-window layout.
- Reconciled the scripting backlog against the bundled mIRCKB and implemented
  `/abook`, `/events on|off`, scripted `/links`, and `$fullscreen`.

## 🗂️ 26.8.6 — Context popup management

- Split the Popups editor into Server/status, Channel, Nick list, Query, Custom
  window and Combined/legacy sections with a Remote-style side menu.
- Added independent On/Off controls and deletion for every popup section.
- Starter popup sections are now created and compiled automatically the first
  time the Popups editor is opened instead of appearing as unsaved examples.
- Dedicated user popup files now always appear above popup entries contributed
  by ordinary Remote scripts, regardless of script filename/load order.
- Retained and regression-tested the complete native nick-list menu whenever no
  script supplies custom nick-list popup entries.
- Channel popup entries using `/channel` now open Channel Central for the
  selected channel instead of sending an unknown command to the IRC server.
- The connection editor now remains open when its backdrop is clicked, avoiding
  accidental dismissal while using the native paste menu.

## 📝 26.8.5 — Aliases and popups editor

- Added mIRC-style Aliases, Popups and Remote tabs to the script editor.
- Added guided starter sources for `aliases.mrc` and `popups.mrc`; both remain
  normal portable mSL files compiled with every other script.
- Kept general remote scripts in their existing multi-file browser while
  reserving the dedicated files for their focused editors.

## 🧳 26.8.4 — Portable client commands

- Added `/markasread [name]` for clearing one window or every unread window on
  the current connection.
- Added persistent `/strip [+-buriec]` message-format stripping controls.
- Added `/tnick`, delayed `/pop` and `/pvoice` with live privilege checks, and
  `/qmsg`/`/qme` broadcasts to all open query windows.

## 🖥️ 26.8.3 — Live script UI and keyboard state

- Added live `$toolbar`, `$treebar`, and `$switchbar` identifiers that follow
  the application layout instead of returning fixed compatibility values.
- Added `/toolbar on|off` for globally showing or hiding the script toolbar
  without changing its buttons or their properties.
- Added event-local `$keychar`, `$keyval`, and `$keyrpt` values for
  `on KEYDOWN` and `on KEYUP`, including browser key codes and held-key repeat
  state.

## 🧩 26.8.2 — Portable mSL compatibility follow-up

- Added live `$bindip` and `$passivedcc` identifiers backed by the active DCC
  listener configuration.
- Added `/pdcc on|off` compatibility as an alias of jIRC's existing passive-DCC
  control.
- Refreshed the implementation audit, roadmap and granular parity checklist
  through the networking and script-UI releases.

## 🧰 26.7.97 — Richer script UI integration

- Expanded `/panel` with inputs, checkboxes, progress rows and separators in
  addition to safe text and command buttons.
- Added enabled, visible and checked toolbar-button properties, toolbar
  separators, and script-controlled treebar width and left/right placement.
- Implemented styled `/linesep` markers in status, channel, query and custom
  windows.
- Added `on KEYDOWN` and `on KEYUP` with key/modifier/input parameters while
  retaining haltable `on TABCOMP` before normal nickname completion.
- Added `on WAVEEND` for `/splay` and `/sound` media completion and `on PLAYEND`
  when a `/play` queue item finishes.

## 🌐 26.7.96 — Networking compatibility

- Added SOCKS4/SOCKS4a alongside SOCKS5, including SOCKS4 user IDs, saved proxy
  selection, and per-server local-address binding.
- Separated DCC's advertised address from its local listener bind address,
  normalized reversed port ranges, and retained standard plus passive/reverse
  CHAT, SEND, RESUME, and DCC Server controls.
- Added real socket-level SOCKS4a, local-bind, DCC range/interface regression
  coverage and an external mIRC/other-client interoperability checklist.
- Fixed auto-protect sending a redundant re-op when a batched MODE line had
  already restored the protected user's operator status.
- Deferred identd until a supported older network demonstrates a real need for
  the additional inbound service and firewall/privacy surface.

## 🧩 26.7.95 — mSL compatibility and lifecycle

- Added persistent `/load` and `/unload` script lifecycle controls, scoped
  `on LOAD`/`on UNLOAD` events, `/remote on|off`, and completed the local IAL
  control commands.
- Added `/help`, `/log`, `/logview`, and `/queryrn`, plus `$parms`, live server
  address, negotiated TLS version, certificate validity, and certificate hash
  identifiers.
- Added `on ACTIVE`, `on DNS`, and `on TABCOMP`; scripts can halt tab completion,
  and DNS events expose `$raddress` and `$dns()` results.
- Added regression coverage and refreshed the in-app help and mIRC parity audit.

## 🔤 26.7.94 — Application-wide fonts

- Applied the selected font throughout jIRC, including server lists, chat,
  nicklists, dialogs, message controls, and detached windows.
- Enforced an 8-pixel minimum custom font size in Settings, `/font`, and
  previously saved settings. Reset still restores the theme default.

## 🪟 26.7.93 — Custom-window echo routing

- Fixed `/echo @window text` and switched forms such as
  `/echo -t @window text` so output appears in the intended custom window.
- Kept echoed lines in the authoritative window buffer so `$line()` and
  `$window().lines` immediately reflect what is displayed.
- Added regression coverage based on the `@i7` and `@i7bot` debug-window usage.

## ✍️ 26.7.92 — Cleaner message spelling menu

- Reordered and condensed the message-box menu, keeping colour palettes hidden
  until Text colour or Background is selected.
- Added one-click suggestions for common misspellings, including corrections
  such as `helo` and `hellp` to `hello`.
- Added optional automatic correction of conservative common typing mistakes.
  Shift+right-click opens the platform WebView's full spelling menu.

## 🌐 26.7.91 — mSL runtime and network state

- Added `$portfree(port[,ip])` with IPv4 and IPv6 interface support.
- Added `$status` values for disconnected, connecting, and connected sessions.
- Added `$remote` handler flags and the steady-state `$starting` and `$exiting`
  process identifiers.

## ⏱️ 26.7.90 — mSL time and runtime identifiers

- Added `$timestamp`, `$timestampfmt`, `$logstamp`, and `$logstampfmt` using
  jIRC's current local-time display format.
- Added `$uptime(mirc|server,N)` with milliseconds, duration, compact duration,
  and seconds return modes. `$uptime(system)` returns `$null` because a reliable
  cross-platform system boot clock is not available through the current runtime.
- Added `$onlineserver`, `$onlinetotal`, and high-resolution `$ticksqpc`
  compatibility.

## 📐 26.7.89 — mSL geometry identifiers

- Added `$intersect()` for line, ray, and segment intersection coordinates.
- Added `$onpoly()` polygon overlap detection, including crossing edges,
  touching boundaries, and fully-contained polygons.
- Added regression coverage for intersection bounds and overlapping,
  contained, and separate polygons.

## 🧩 26.7.88 — Script-controlled client UI

- Implemented `/editbox` with active/status/window targeting, focus, appended
  spacing, selection ranges, and submit support.
- Implemented `/timestamp`, `/switchbar`, `/treebar`, and `/font` against
  jIRC's existing timestamp modes, layouts, and installed-font settings.
- Implemented `/clearall` buffer-type switches and `/close` query, DCC chat,
  status, custom-window, wildcard, and connection scoping.

## 🎨 26.7.87 — jIRC message-box menu

- Replaced the generic WebView/browser right-click menu in the message box with
  a focused jIRC menu, removing password import, device sharing, and other
  unrelated browser actions.
- Added selection-aware emoji insertion, formatting buttons, text/background
  colour palettes, persistent Apply/Reset controls, and a spell-check toggle.
- Kept standard Undo, Cut, Copy, Paste, and Select all actions in the new menu.

## 🪟 26.7.86 — Script window buffer commands

- Implemented `/titlebar` for custom `@windows`, including live title updates
  in docked and detached window views.
- Implemented sandboxed `/loadbuf` and `/savebuf` with mIRC-compatible
  append/replace switches and line ranges for UTF-8 files under
  `jIRC/scriptdata`.
- Added backend and frontend coverage for persistence, line restoration, and
  stable window identity.

## 📇 26.7.85 — Address book and user notes

- Added a searchable local address book for nick, network, real name, email,
  website, and private free-form notes.
- Contacts can be opened from either main layout and directly from a channel
  nick's context menu, which pre-fills the nick and network.
- Entries persist in <code>addressbook.json</code> under the active jIRC data
  folder, migrate defensively from the earlier WebView draft, and can be edited
  or removed.

## ▶️ 26.7.84 — Perform commands on connect

- Saved server profiles now include an ordered, multi-line Perform command list.
- Commands run through the normal mSL command and alias engine after
  `on CONNECT` and before automatic channel joins, with identifiers such as
  `$me` available.
- Existing profiles remain compatible and default to an empty Perform list.

## ✍️ 26.7.83 — Message spell checking

- Added platform-native spell checking to the message box, including the normal
  misspelling underline and right-click correction menu supplied by each OS.
- Settings → Appearance can disable checking or select an installed dictionary
  language, with System language as the cross-platform default.
- Existing settings migrate with spell checking enabled.

## 🔔 26.7.82 — Startup update notification

- jIRC now checks once when the main app starts and notifies the user only when
  a newer signed release is available.
- Startup checks never download, install, or restart automatically, and stay
  silent when jIRC is current or the update service cannot be reached.
- Detached chat windows and React remounts share the same launch check, avoiding
  duplicate update requests and notifications.

## 🪟 26.7.81 — Wider responsive settings

- Widened the Settings dialog while keeping it within the available app window.
- Settings tabs now wrap instead of requiring horizontal scrolling, and Alerts
  sound paths/buttons stay contained at desktop and narrow window sizes.

## 🔐 26.7.80 — Stop repeated macOS Keychain prompts

- Removed the destructive Keychain availability probe previously run whenever
  the Connect dialog opened.
- Cached successful, missing, and denied credential lookups for the jIRC
  session, so Connect and Auto-join cannot repeatedly request the same macOS
  Keychain item.

## ℹ️ 26.7.79 — About dialog and dedicated DCC settings

- Added an About popup showing the running jIRC version, with a **Help me**
  button that opens the bundled Help guide.
- Moved all transfer, passive-mode, port, address, and DCC Server controls from
  Behaviour into a dedicated DCC settings tab.

## 🔊 26.7.78 — Notification sounds and audio commands

- Added configurable sounds for mentions, private messages, invites, and watched
  users coming online, with built-in tones, per-device audio-file selection,
  volume control, test buttons, and optional quiet hours.
- Implemented sandboxed mSL `/sound` and `/splay` playback, including pause,
  resume, and stop controls.

## 🎛️ 26.7.77 — Complete channel-mode editor

- Rebuilt Channel Central around each server's advertised `CHANMODES`, `PREFIX`,
  and `MODES` rules instead of a fixed set of checkboxes.
- Added dynamic flag and parameter controls plus ban, ban-exception, and
  invite-exception list management.
- Mode changes are validated and split into correctly ordered, server-sized
  `MODE` batches, with a new Modes button directly on channel windows.

## 🧪 26.7.76 — Updater test release

- Test release for verifying the complete signed in-app update path from
  version 26.7.75.

## 🔄 26.7.75 — Signed application updates

- Added a cross-platform signed updater under Settings → Behaviour, including
  update checks, release notes, download progress, installation, and restart.
- Added GitHub release automation for Windows, macOS, and Linux, producing the
  signed update artifacts and `latest.json` consumed by installed clients.

## 🎯 26.7.74 — Cleaner composer reset

- Removed the empty colour swatch box and shortened the emoji picker button to
  its face icon so the composer controls fit together more cleanly.
- Reset now restores the colour selectors to black text with no background as
  well as disabling persistent message colours.

## 🎯 26.7.73 — Persistent composer colours

- Applying text/background colours now keeps that combination active for every
  normal message until Reset is clicked.
- Each outgoing coloured message is safely terminated with a reset code, while
  slash commands remain untouched.
- The Apply button clearly reports and highlights its active state.

## ✨ 26.7.72 — Polished composer and system font picker

- Rebuilt the composer into a dedicated toolbar above the message field, with
  Emoji and text-style controls grouped cleanly.
- Replaced unexplained numeric colour boxes with labelled **Text colour** and
  **Background** selectors, named colours, swatches, and an Apply colours preview.
- Settings now discovers and lists installed font families through one
  cross-platform Windows/macOS/Linux/BSD backend instead of requiring manual entry.

## 🎨 26.7.71 — Input formatting toolbar

- Added optional message-input controls for mIRC bold, italic, underline,
  foreground/background colour, and formatting reset codes.
- Formatting wraps selected text or inserts at the caret, preserving focus and
  selection for continued typing.
- Added a Settings → Appearance toggle and responsive compact behavior for
  narrow windows.

## 📝 26.7.70 — README refresh

- Brought the public project overview up to date with current IRCv3, IRCX,
  authentication, DCC, detachable-window, settings, and scripting support.
- Corrected outdated claims about window layout, keyring fallback, application
  data, the help guide's scope, and runtime `/alias` syntax.
- Expanded the scripting summary to cover the editor, picture windows,
  dialogs, managed WebViews, panels, sockets, and DCC Server.

## 📚 26.7.69 — Help guide refresh

- Audited the complete built-in guide against the current client and scripting
  engine.
- Documented detached chat/script windows, editor themes and diagnostics,
  settings pages, current authentication, newer script events, DCC Server, and
  the complete runtime `/alias` syntax.
- Removed outdated claims about `/clear`, dialog support, identifier properties,
  and unconditional keyring availability.

## 🔐 26.7.68 — Broader IRCv3 and IRCX authentication

- Added TLS-only **SASL OAUTHBEARER** authentication. Access tokens use the
  existing account-password field and OS-keyring storage.
- Added the IRCX **ANON** pre-registration authentication package alongside
  native NTLM and script-managed authentication.
- Negotiates IRCv3 `account-tag`, `batch`, `labeled-response`, and
  `draft/chathistory`, including the required batch dependencies.
- Account tags now keep IAL account metadata current for every user message,
  while structural `BATCH` delimiters stay out of chat buffers.

## 🔌 26.7.67 — Complete DCC Server mode

### Added
- Added the mIRC-compatible direct DCC Server listener with Chat, Send, and
  Fileserver services, configurable port and independent service switches.
- Added direct DCC Server clients: `/dcc chat IP[:port]`,
  `/dcc send IP[:port] file`, and `/dcc fserve IP[:port]`.
- Added `/dccserver [+|-scf] [on|off] [port]`, `$dccport`, and fully selected
  `on DCCSERVER` events with `$nick`, `$address`, and `$filename`.
- `/halt` in `on DCCSERVER` rejects the request before any chat, file, or
  fileserver session is opened.
- Added persisted DCC Server settings and safe received-file naming under the
  existing jIRC DCC download directory.

### Compatibility and safety
- Implements protocol replies 101/111/121 and unavailable/rejected responses,
  the 15-second initial request timeout, resume positions for direct sends, IPv4,
  IPv6, explicit non-default ports, and filenames containing spaces.
- Incoming services are disabled until the user explicitly enables DCC Server;
  individual Chat, Send, and Fileserver services can be disabled separately.
- Updated HelpMe, README, parity, implementation audit, and roadmap status.

### Verified
- Focused DCC protocol/parser/script-event tests, frontend type-check and
  production build, complete frontend and non-live Rust suites, and a Tauri
  release build.

---

## 🧩 26.7.66 — Complete cross-platform script dialogs

### Added
- Added mIRC-style dialog table parsing alongside jIRC's concise syntax,
  including size declarations and numeric control IDs.
- Added radio, group box, scroll, editable combo, icon, link, tab, menu, and
  menu-item controls to the existing text/edit/button/check/list controls.
- Added portable control styles for password/read-only/multiline edits,
  multi-selection, initial disabled/hidden state, tabs, ranges, default/OK and
  cancel behavior.
- Expanded `/dialog` with modeless table instances, title, size, rename, and
  close operations.
- Expanded `/did` with add/insert/replace/delete/clear, enable/disable,
  show/hide, focus/default, check/uncheck/indeterminate, range, ID lists and
  ranges; added `/didtok`.
- Added `$dname`, `$devent`, modeless `$dialog()` state, richer `$did`
  properties, `$didwm`, `$didreg`, and `$didtok`.
- `on DIALOG` now matches dialog name, event (`init`, `edit`, `sclick`,
  `dclick`, `menu`, `scroll`, `close`) and individual/list/range control IDs.

### UI and documentation
- Dialogs are themed and resizable, support Enter/Escape default/cancel
  behavior, and load icon controls from the script-data sandbox.
- Corrected stale README wording: DCC chat and ordinary file transfer are
  implemented; DCC server mode remains roadmap work.
- Updated HelpMe and the roadmap to describe the completed portable dialog API.

### Verified
- Focused parser, event, identifier, and frontend state coverage; complete
  frontend and non-live Rust suites; production frontend and Tauri release
  builds.

---

## 🎨 26.7.65 — Complete cross-platform picture drawing

### Added
- Added `/drawcopy`, `/drawpic`, `/drawrot`, `/drawscroll`, and `/drawsave`.
- Image copy/load operations support crop, resize, tiling, transparent colours,
  smoothing, cross-window sources, quoted sandbox paths, and image binvars.
- `/drawsave` writes sandboxed BMP, PNG, and JPEG files and supports `-v`
  binary-variable output.
- Completed rounded rectangles, surface/border and patterned fills, clipped and
  styled text, deferred `-n` drawing, rotation backgrounds/fitting/clipping,
  colour replacement regions, and multi-region scrolling.
- Added `$click`, `$getdot`, `$inrect`, `$inellipse`, `$inroundrect`, `$inpoly`,
  `$width`, `$height`, and the remaining portable `$mouse` properties.
- Listbox `lbclick` now fires from actual row selection; picture clicks no
  longer incorrectly fire it.
- Expanded the built-in HelpMe guide with picture-command syntax, switches,
  identifiers, mouse/listbox events, examples, sandbox rules, and exclusions.

### Security and portability
- Picture files remain inside `jIRC/scriptdata`.
- Windows-native `/drawdll` remains intentionally unsupported.

### Verified
- Focused picture command/identifier coverage, complete frontend and non-live
  Rust suites, production frontend build, and full Tauri release build.

---

## 🪟 26.7.64 — Custom-window interaction completion

### Added
- Picture-window `menu @window` mouse actions now run for `mouse`, `sclick`,
  `dclick`, `uclick`, `rclick`, `lbclick`, and `leave`.
- `$mouse.x`, `$mouse.y`, `$mouse.win`, and `$mouse.lb` are available while a
  custom-window mouse action runs.
- `/drawfill` flood fills a picture canvas and `/drawreplace` replaces an exact
  colour across it.

### Fixed
- Custom `@window` right-click menus now use their own `menu @window`
  definition instead of the status menu.
- Mouse-action entries are kept out of the visible right-click menu.

### Verified
- Focused script-engine coverage, complete frontend and non-live Rust suites,
  production frontend build, and full Tauri release build.

---

## ⏱️ 26.7.63 — Minute timestamp dividers

- Divider timestamp mode now inserts a rule only on the first message of a new
  minute instead of repeating it for every message.
- Messages received during the same minute remain grouped beneath that minute's
  timestamp divider.

### Verified
- Focused minute-boundary coverage, complete frontend and non-live Rust suites,
  production frontend build, and full Tauri release build.

---

## 🕒 26.7.62 — Timestamp display modes

### Added
- Settings → Appearance now offers three timestamp layouts: inline
  (`timestamp nickname message`), a centered timestamp divider above each
  message, or timestamps completely off.
- Existing saved `Show timestamps` preferences migrate automatically to inline
  or off.

### Verified
- Complete frontend tests, production frontend build, complete non-live Rust
  suite, and full Tauri release build.

---

## 🖼️ 26.7.61 — Picture-window drawing

### Added
- `/window -p @name` now renders a persistent, detachable HTML canvas instead
  of a text buffer.
- Core cross-platform mSL drawing is supported with `/drawsize`, `/drawdot`,
  `/drawline`, `/drawrect` (including `-f` and `-e`), and `/drawtext`.
- Drawing operations are retained and replayed when the window rerenders or is
  detached, with mIRC palette numbers and RGB values supported.

### Verified
- Focused picture-window engine/store tests, complete frontend and non-live
  Rust suites, production frontend build, and full Tauri release build.

---

## 🪟 26.7.60 — Interactive custom windows

### Added
- `/window -e @name` now renders a real editbox while listbox and picture
  windows no longer show an inappropriate chat input.
- Custom listbox rows can be selected with the mouse; Ctrl/Cmd-click adds to
  the selection.
- `/sline`, `$sline(@name,N)`, `$sline(...).ln`, and
  `$line(@name,N).state` expose and control the same one-based selection state.
- Insert, delete, and clear operations keep script and visible selections in
  sync.

### Verified
- Focused custom-window engine/store tests, complete frontend and non-live Rust
  suites, production frontend build, and full Tauri release build.

---

## 🎨 26.7.59 — Selectable mSL editor themes

### Added
- The script editor now defaults to familiar VS Code Dark+ syntax colours.
- A persistent editor-theme selector offers VS Code Dark+, VS Code Light+,
  Monokai, and Solarized Dark.
- Theme changes apply immediately in both the docked and detached script
  editors without changing the main jIRC theme.

### Verified
- Editor-theme coverage, complete frontend test suite, production frontend
  build, complete non-live Rust suite, and full Tauri release build.

---

## 🪟 26.7.58 — Client-state scripting identifiers

### Added
- `$appactive` reports whether any jIRC window is focused, while `$appstate`
  reports the main window as `normal`, `minimized`, `maximized`, `full`, or
  `hidden`.
- `$darkmode` follows the theme actually displayed by jIRC, including the
  resolved operating-system theme when Settings uses `system`.
- `$notify`, `$notify(0)`, `$notify(N/nick)`, `.ison`, and `.addr` expose the
  configured notify list and its current ISON-backed online state.

### Verified
- Focused scripting tests, complete non-live Rust suite, frontend tests and
  production builds.

---

## 🧭 26.7.57 — Sandboxed path comparison

### Added
- `$samepath(path1,path2)` compares paths within `scriptdata`, resolving
  existing paths and normalizing nonexistent ones.
- Comparisons follow Windows case-insensitivity and Unix case-sensitivity while
  preserving jIRC's traversal-safe leaf-name sandbox.

### Verified
- Focused sandbox/platform path tests, complete non-live Rust suite, and
  production frontend/help build.

---

## 🧳 26.7.56 — Process and portable identifiers

### Added
- `$portable` reports whether the running jIRC executable has a `portable.txt`
  marker beside it, matching jIRC's actual portable-install rule.
- `$cmdline` returns the launch arguments in their original order.

### Verified
- Pure portable/command-line tests, an engine expansion test, the complete
  non-live Rust suite, and production frontend/help build.

---

## 💬 26.7.55 — Query window identifier

### Added
- `$query(0)`, `$query(N)`, and `$query(nick)` enumerate open query windows for
  the current connection, excluding status, channel, DCC, and custom windows.
- `$query().wid`, `.cid`, `.addr`, and `.idle` use the live window registry,
  connection IDs, IAL, and channel activity state.

### Verified
- Focused multi-server query test, complete non-live Rust suite, and production
  frontend/help build.

---

## 🪟 26.7.54 — Previous active window identifiers

### Added
- `$lactive`, `$lactivewid`, and `$lactivecid` now report the previously
  focused window and its stable window/connection IDs across networks.
- Closing the previously active window safely clears these identifiers.

### Verified
- Focused window-registry test, complete non-live Rust suite, and production
  frontend/help build.

---

## 🧮 26.7.53 — Alias file identifier

### Added
- mIRC-compatible `$alias(N/filename)`: count loaded alias files, return the Nth
  filename, or test a filename case-insensitively. Event-only script files are
  correctly excluded.

### Verified
- The focused `$alias` compatibility test, complete non-live Rust suite, and
  production frontend/help build pass.

---

## 🧩 26.7.52 — Alias compatibility & detachable script editor

### Added
- A larger, user-resizable mSL editor with line numbers, syntax highlighting,
  bracket and quote diagnostics, lint markers, draft recovery, and
  <kbd>Ctrl</kbd>+<kbd>S</kbd> saving.
- The script editor can be popped out into its own resizable operating-system
  window and used independently from the main jIRC window.
- Script aliases can be bound to F1–F12, Shift+F-key, and Ctrl+F-key shortcuts.

### Fixed
- `/alias` now accepts mIRC-compatible `/alias [-l] [filename] <name> [command]`
  syntax, normalizes leading slashes, evaluates definitions at the correct
  time, persists local aliases, and safely stops direct or indirect recursion.
- The standalone browser development preview remains usable when Tauri window
  metadata is unavailable.

### Verified
- Alias compatibility tests, frontend tests and production build, the complete
  non-live Rust suite, and the full Tauri release build.

---

## 🧰 26.7.51 — mIRC parity, script UI, DCC fserve & flood protection

### Added
- **Script-defined toolbar buttons and docked panels.** Scripts can add safe
  application controls with `/toolbar` and `/panel`; button commands retain
  their defining script context and delayed `$!` evaluation.
- **Sandboxed DCC file server.** `/fserve <nick> <maxgets> <homedir> [welcome]`
  offers a standard DCC chat browser with `dir`, `cd`, `pwd`, `get`, and send
  slots. Served files and welcome text are confined to `scriptdata`, including
  canonical-path and traversal checks.
- **Configurable outbound flood protection.** Settings → Server can enable or
  disable throttling and choose the message/window limits (default: four
  user/script lines per two seconds). Registration, authentication, and
  protocol-generated replies bypass the user-output limiter.
- A broad **mIRC/mSL parity pass** covering richer aliases/events/identifiers,
  timed variables and hash tables, playback queues, binary and TLS sockets,
  managed webviews, IAL/WHOX state, IRCv3 capability handling, SASL
  EXTERNAL/SCRAM-SHA-256, TLS client certificates, and expanded DCC
  resume/passive/retry behaviour.

### Fixed
- Windows test executables now carry the Common Controls v6 manifest dependency,
  so the documented backend test command links and runs correctly.

### Verified
- Frontend tests and production build, the complete non-live Rust suite, and the
  full Tauri release build pass with the new wiring.

---

## 🧬 26.7.50 — Binary socket reads (`/sockread &binvar`)

- Scripts can now read **binary** socket data byte-for-byte: **`/sockread &binvar`** (inside `on SOCKREAD`) puts the line's exact bytes into a binary variable, with no text/UTF-8 mangling. Parse it with `$bvar` / `$bfind`, build replies with `bset`, and send them with `/sockwrite name &binvar`. This is what binary protocols and crypto handshakes need — the text `/sockread %var` form is unchanged. New **Help → Sockets** section explains it with a before/after example.

---

## 🌉 26.7.49 — `/server` works from scripts (bridges can connect the client)

- Implemented **`/server [-m] <host> <port> [password]`** as a script command. Previously a script's `/server` fell through to a raw `SERVER` line (which went nowhere), so a script that stands up a **local bridge/proxy** and then does `/server 127.0.0.1 <port> <key>` could never get the client to connect. Now it opens a server window and connects the native client — so the bridge's listener accepts, and its `on SOCKLISTEN` / `on SOCKREAD` handlers finally run.

---

## 🔌 26.7.48 — `/socklisten -d <ip>` fixed (local bridges connect)

- Fixed **`/socklisten -d <bindip> <name>`** — jIRC was treating the bind IP (e.g. `127.0.0.1`) as the **socket name**, so the listener registered under the wrong name and `$sock(<name>).port` came back **blank**. Scripts that set up a local proxy/bridge and then do `/server 127.0.0.1 $sock(<name>).port` now get the real port and connect. (mIRC's full `/socklisten [-d] [bindip] <name> [port]` syntax is parsed correctly.)

---

## 📏 26.7.47 — Popup menus fit their content

- Popup menus now **grow to fit the widest item on one line** instead of wrapping long labels onto a second row and looking cramped.
- mIRC's **tab hints** (a `$chr(9)` in a menu label, e.g. `Take` + `- rotate keys`) render as a **dimmed, right-aligned hint** rather than collapsing into the label.

---

## 🖱️ 26.7.46 — Popup menus: multi-line commands & `: { }` form

- Fixed popup (`menu`) items whose command is a **multi-line `{ … }` block** (e.g. a `while`/`if` loop across several lines). The parser was reading popups line-by-line, so those items shattered into one broken entry per line; now the whole block stays with its item and runs correctly.
- Fixed the **`Label : { command }`** form (colon *and* braces) — the label no longer keeps a stray trailing `:`, and a `: command` that contains its own `{ }` (like `: if (x) { … }`) is no longer mistaken for a block.

---

## 🔵 26.7.45 — Icon recoloured to the accent blue

- Recoloured the app icon to sit in jIRC's **blue** accent (`#7aa2f7`) instead of leaning purple — the white `#` on a blue gradient now matches the rest of the UI.

---

## 🎨 26.7.44 — Proper app icon + tidy-ups

- **New app icon** — a blue→purple gradient with a bold white `#` (the IRC channel symbol), matching jIRC's accent colours. Replaces the default blue-box placeholder, across Windows/macOS/Linux (and the mobile/store icon sets).
- **Fixed the bundle-identifier warning** — the identifier no longer ends in `.app` (which clashes with the macOS `.app` bundle extension). Your saved servers and passwords are unaffected (they live in the `jIRC` folder and the OS keyring, not under the identifier).
- **Removed** the old one-time `com.jirc.app → jIRC` data-folder migration — no longer needed.

---

## 🚪 26.7.43 — "New connection" chooser is back

- Clicking **＋ Add a connection** now opens the two-option chooser again — **Connect to a server** or **Open a local console** — the same choice you get on the startup screen, instead of jumping straight into the connect form. (Esc or a click outside closes it.)

---

## 🛰️ 26.7.42 — Channel detection is purely ISUPPORT-driven

- Reverted the hardcoded `%#`/`%&` channel-prefix special-casing from 26.7.40. Whether a name is a channel is now decided **entirely by the server's advertised `CHANTYPES`** (from ISUPPORT/005) — no client-side assumptions. IRCX servers list their `%#`/`%&` prefixes there (e.g. `CHANTYPES=%#`), so `%#` channels still work exactly as before. `$chan` still returns the full name **with** the `%#` prefix on IRCX (it always did — it's the raw channel name, unlike mIRC which drops it).

---

## 🧊 26.7.41 — Every dialog path unfrozen + a thank-you

- **Audited and fixed every remaining dialog freeze.** 26.7.40 fixed aliases/commands; this covers the rest — custom `/dialog` handlers (`on DIALOG`), `on INPUT`, `on OPEN`/`on CLOSE`, `on NOTIFY`, and right-click menu building. Any script path that can pop an `$input`/`$?` prompt now runs off the UI thread, so the prompt can never freeze the app. (Confirmed these are the *only* places an engine run can block the UI.)
- **Thanks:** added **xpu|se** to the credits for the hands-on testing and bug reports behind the recent fixes.

---

## 🧊 26.7.40 — Dialog freeze fix + IRCX `%#` channels

- **Fixed the frozen `$input` / `$?` dialog.** Running an alias that shows an input prompt from the input bar (e.g. `passx` with `mode $me +h $?="Enter Password"`) locked up the whole app — the dialog appeared but you couldn't type, cancel, or click anything. The alias now runs on a worker thread, so the prompt blocks the *script* and not the UI (the same way right-click popup commands already worked).
- **`%#` and `%&` channels** are now treated as channels everywhere `#` is — even when the server doesn't advertise `%` in its CHANTYPES. Fixes channel modes on a `%#` channel being misread as user modes, and `%#`/`%&` buffers rendering as a query instead of a channel. `/part %#chan` and `/channel %#chan` recognize the prefix too.

---

## 🩹 26.7.39 — Multi-word `$?` prompts

- Fixed **`$?="Enter Password"`** and other multi-word input prompts — the whole message is kept now (it used to get cut off and leave stray text behind). `$input` benefits too.

---

## ⏱️ 26.7.38 — `$timer` + protect enforcement

- **`$timer`** lets scripts check running timers — how many, a timer's command, its remaining reps, its delay.
- **Protect now acts**: if someone deops a person on your protect list in a channel you run, jIRC re-ops them automatically. That finishes the auto-op / auto-voice / protect feature.

---

## 🎨 26.7.37 — Colour & number identifiers

- **`$rgb`** (convert R,G,B ↔ mIRC colour number), **`$ansi2mirc`** (turn ANSI colour codes into mIRC ones — handy for relaying ANSI text), and **`$bits`** / **`$numbits`**. This closes out the pure-logic identifier gaps.

---

## ↩️ 26.7.36 — `$!` last-input value

- Added **`$!`** — after a `$?`/`$input` prompt, `$!` gives you back what was typed (no need for a temp variable). `$!name` also works as delayed evaluation (the literal `$name`).

---

## ❓ 26.7.35 — The classic `$?` input prompt

- Added **`$?`** — the old-style input identifier (`$?="Pick one"`, `$?*=` for passwords, `$?!=` for yes/no, `$$?` to require an answer). Scripts written with `$?` instead of the newer `$input` now work.

---

## 🖥️ 26.7.34 — Manage users in Settings

- A new **Settings → Users** tab to see and edit your access list and auto-op / auto-voice / protect lists — no need to remember `/auser` and `/aop` syntax. Auto-op entries are **grouped by network**, so multi-server setups stay clear.
- Anything you change in the UI is the same list your scripts see, and it's saved to disk.

---

## 💾 26.7.33 — User lists saved to disk (subsystem complete)

- Your **user list and auto-op/voice/protect lists now survive restarts** — they're saved to `users.json` whenever they change and loaded on startup. `/auser`, `/aop`, and friends are finally permanent.
- That wraps up the whole user-access subsystem: manage users with levels, gate events by level (`on 10:TEXT:…`), auto-op/voice on join, and keep it all across sessions.

---

## 🎩 26.7.32 — Auto-op / auto-voice / protect (user list part 3)

- **`/aop`**, **`/avoice`**, **`/protect`** lists with **`$aop`**/**`$avoice`**/**`$protect`** to query them. `/aop on` then `/aop *!*@friend.com #chan` and jIRC auto-ops matching people when they join a channel you run (auto-voice too).
- Protect's list is queryable now; its re-op-when-deopped enforcement comes next, along with saving all these lists to disk.

---

## 🚦 26.7.31 — Access-controlled events (user list part 2)

- **Level-gated events**: `on 10:TEXT:!op &:#:{ mode # +o $2 }` now only triggers for users you've given level 10+. Also `on @:` (ops only), `on =5:` (exactly level 5), and named levels like `on admin:`.
- **`$ulevel`** / **`$clevel`** tell you the user's level and the event's level, and **`/guser`** adds someone by looking up their address automatically.

---

## 👥 26.7.30 — User access list (part 1)

- The start of mIRC's **user list**: **`/auser`**, **`/ruser`**, **`/iuser`** to manage users with access levels, and **`$ulist`** / **`$level`** to query them. e.g. `/auser 10 *!*@friend.com` then `$level(nick!u@friend.com)` → `10`.
- Next up in this subsystem: level-gated events (`on 10:TEXT:...`), auto-op/voice/protect lists, and saving the list to disk.

---

## 🔎 26.7.29 — `$var` variable lookup

- **`$var(%prefix*, N)`** lets a script list its own variables — count them (`N=0`), get the Nth name, or read `.value`. Handy for "unset everything matching" or debugging.

---

## 🏷️ 26.7.28 — `$prop` for custom identifiers

- **`$prop`** lets your own identifiers read the `.property` they were called with — e.g. `$temp(20).celsius` vs `$temp(20).fahrenheit` can now do different things.
- **`$unsafe`** is accepted (it's a no-op in jIRC, which never double-evaluates).

---

## 🔧 26.7.27 — `$(...)` and length limits

- **`$(...)`** — the short form of `$eval`, so `$(%x, 2)` re-evaluates a value (handy for dynamic lookups).
- **`$maxlenl` / `$maxlenm` / `$maxlens`** — the safe text-length limits (10240 / 2048 / 512) for scripts that split long messages.

---

## 🧮 26.7.26 — Dynamic variables

- The classic **`%color. [ $+ [ $nick ] ]`** pattern works now — build a variable name on the fly and read it. Great for per-user or per-channel data (`%greet. [ $+ [ $nick ] ]`) and array-style loops (`%item. [ $+ [ %i ] ]`).
- Done carefully so nothing else changes: only this exact shape is treated specially; every other use of `[ ]` behaves exactly as it did before.

---

## 🎚️ 26.7.25 — `$show` and `$result`

- **`$show`** lets an alias tell whether it was run normally or silently (with a `.` prefix) — so it can be chatty or quiet to match.
- **`$result`** gives you the value the last alias `/return`ed, however it was called.

---

## 🔡 26.7.24 — Case-sensitive identifiers, completed

- Added the rest of the exact-match family: **`$matchtokcs`**, **`$wildtokcs`**, **`$remtokcs`**, **`$reptokcs`**, **`$addtokcs`**, **`$sorttokcs`**, **`$replacexcs`**. Every text identifier with a case-sensitive form in mIRC now has one.

---

## 🔠 26.7.23 — Case-sensitive identifiers

- Added exact-match versions of common text identifiers: **`$istokcs`**, **`$findtokcs`**, **`$replacecs`**, **`$removecs`**, **`$poscs`**, **`$countcs`** — for when upper/lower case matters.

---

## 🧭 26.7.22 — Token tweaks

- **`$puttok`** and **`$instok`** accept negative positions now (`-1` = from the end), matching `$gettok`.
- **`$read(file, s, word)`** matches whole words — `s, yes` no longer accidentally matches a line starting with `yesterday`.

---

## 📖 26.7.21 — `$read` can search files

- **`$read`** now searches: `$read(file, w, *pattern*)` finds the first line matching a wildcard, `$read(file, s, text)` finds the first line starting with some text, and `$read(file, r, regex)` uses a regex.
- **`$readn`** tells you which line number matched — so you can loop through every match in a file.

---

## 🎯 26.7.20 — The `&` word wildcard

- Matchtext now understands **`&`** — a standalone `&` matches exactly one word. The classic `on *:TEXT:!weather &:#:` (trigger on `!weather london`, not on `!weather` by itself) finally works as it does in mIRC.

---

## 🔤 26.7.19 — `$sorttok` by rank + `returnex`

- **`$sorttok(..., c)`** sorts a list by channel prefix (owner, admin, op, half-op, voice, then the rest) — handy for tidy nick lists.
- **`returnex`** now works as an alias for `return` (jIRC's `return` already keeps your spaces intact).

---

## ✂️ 26.7.18 — Sharper `$mid` and `$strip`

- **`$strip(text, c)`** can now remove just the thing you ask for (colour, bold, underline, …) instead of everything.
- **`$mid`** handles negative positions and lengths like mIRC (count from the end, or drop the last few characters).

---

## 🧮 26.7.17 — `/var` maths + safer, smarter `$iif`

- **`/var` and `/set` do maths**: `var %a 1 + 2` sets `%a` to `3` (one operation, e.g. `+ - * / % ^`). Things that aren't a clean number-operator-number, or use `-n`, stay as text — just like mIRC. The `=` is now optional too (`var %a 1 + 2`).
- **`$iif` conditions** now understand channel operators like `isop`/`ison`, matching `if`.
- **New help section** explaining, in plain English, why other people's text can't turn into commands in jIRC (no double-evaluation).

---

## 🔔 26.7.16 — Notify-list events

- **`on NOTIFY`** and **`on UNOTIFY`** let a script react when a friend on your notify/watch list comes online or goes offline — `$nick` is who changed. e.g. `on *:NOTIFY:/msg $nick welcome back!`

---

## 🪟 26.7.15 — Window events

Scripts can now react when windows open and close:

- **`on OPEN`** and **`on CLOSE`** fire for query (`?`), channel (`#`), and custom (`@name`) windows — e.g. `on *:CLOSE:?:/echo you closed $target`.
- A query window gives you `$nick` (the other person) and `$target` (the window).

---

## 🧩 26.7.14 — Script lifecycle events

Your scripts can now react to the client's own lifecycle:

- **`on START`** — runs once at launch, so a script can initialise itself.
- **`on UNLOAD`** — runs just before a reload, for cleanup.
- **`on EXIT`** — runs as jIRC shuts down, so scripts can **save their data before you quit**.

---

## 🚪 26.7.13 — Clearer channel-rejoin settings

- **Settings → Behaviour** now has two clearly-labelled toggles: **Rejoin channels when kicked** and **Rejoin channels after a disconnect**.
- Rejoin-after-disconnect now **remembers the channels you were in**, so it works even if your channel windows were closed on disconnect (it used to silently do nothing in that case).

---

## 🔢 26.7.10 – 26.7.12 — Multi-server scripting (numeric IDs)

For anyone juggling several IRC / IRCv3 / IRCX / IRCwX connections at once:

- **`$cid`**, **`$scon`**, **`$activecid`** — number your connections and find the current / active one.
- **`$wid`**, **`$activewid`** — number your windows and find the focused one.
- **`/scon N cmd`** and **`/scid cid cmd`** — run a command on *another* connection, in that connection's own context.
- Also: **`$scid`**, **`$version`**, and **`$active`** (the name of your focused window).

---

## ✨ 26.7.9 — The `$iif` glow-up

- Added **`$v1` / `$v2`** — the operands of your most recent comparison.
- **`$iif` now evaluates lazily**, so the everyday `$iif(getvalue, $v1, default)` idiom finally works (and the untaken branch isn't run, matching mIRC).

---

## 🖱️ 26.7.7 – 26.7.8 — Right-click menus & file/ban identifiers

- **Popup menus** got real power: **`$snick` / `$snicks`** (the selected nicks), **`$style`** (checked / disabled items), and **`$submenu`** (dynamically-built menus).
- **`$file(name)`** — file size / times / name / extension.
- **`$banmask`** and a fixed **`$bnick`** (now just the nick part of a ban mask, like mIRC), plus **`$notags`** to strip IRCv3 message tags.
- Rewrote the in-app **Help** (`/help`) to match what the engine actually does.

---

## 🔐 26.7.6 — IRCX channel ownership

- On becoming channel owner, jIRC provisions and stores your **OWNERKEY / HOSTKEY** automatically.
- **Takeover protection**: if someone strips your `+q`, jIRC reclaims ownership with the stored key, clears the owner list, kicks the offender, and rotates the keys.
- Fixed `/mode <nick> +h <key>` so IRCX self-promotion no longer prepends the channel name, and taught the editbox mIRC's `//command` (evaluate-then-run).

---

<div align="center">
<sub>Built with 🦀 Rust + Tauri and an unreasonable devotion to <code>mirc.chm</code>. &nbsp;·&nbsp; The full technical changelog lives in the source tree.</sub>
</div>
