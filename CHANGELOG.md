<div align="center">

# 📜 jIRC — Changelog

**A modern, cross-platform IRC client with a built-in mIRC-style scripting engine.**

Speaks standard IRC (RFC 1459/2812 + IRCv3) and **IRCX** · runs **mSL** scripts natively · MIT licensed

Versions use CalVer (`YY.M.D`) — newest first.

</div>

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
