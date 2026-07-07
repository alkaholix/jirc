<div align="center">

# 📜 jIRC — Changelog

**A modern, cross-platform IRC client with a built-in mIRC-style scripting engine.**

Speaks standard IRC (RFC 1459/2812 + IRCv3) and **IRCX** · runs **mSL** scripts natively · MIT licensed

Versions use CalVer (`YY.M.D`) — newest first.

</div>

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
