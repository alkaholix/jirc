# jIRC version log

What changed in each release. jIRC uses **CalVer** — the version number is the
release date in `YY.M.D` form (e.g. `26.6.25` = 25 June 2026). A 2-digit year
keeps every field ≤ 255 so the Windows MSI installer accepts it. Cut a new
version when a meaningful batch of work lands, and set the same number in all
three places that carry it:

- `src-tauri/Cargo.toml` — the Rust crate version (what the CTCP **VERSION**
  reply reports, via `CARGO_PKG_VERSION`).
- `src-tauri/tauri.conf.json` — the installer / app metadata version.
- `package.json` — the frontend / npm version.

Newest first.

## 26.7.9

**`$v1` / `$v2`** — the operands of the most recent comparison, and with them a
real fidelity fix: **`$iif` now evaluates lazily**. It expands the condition,
publishes `$v1`/`$v2`, then expands only the branch that's taken — so the
ubiquitous mIRC idiom `$iif(getvalue, $v1, default)` finally works (it returns
the value when truthy, the default otherwise), and the untaken branch isn't
evaluated (matching mIRC). `if`/`while` conditions set `$v1`/`$v2` too: for
`a == b` / `a isin b` they're the two operands, otherwise `$v1` is the whole
value tested. Implemented via the same "don't pre-expand args" hook `$regsubex`
already uses, so nothing else changed — all existing tests still pass.

## 26.7.8

More mSL identifiers, working down the mIRC reference: the focused-window
identifier that state-aware scripts lean on, plus the client version.

### Added (mSL identifiers)
- **`$active`** — the name of the window you currently have focused (mIRC's
  status window reads as `Status Window`). The frontend reports the focused
  buffer to the engine on every switch, so `$active` is correct in typed
  commands *and* inside event handlers, not just where a command was run.
- **`$version`** — the jIRC client version (its own CalVer, e.g. `26.7.8` — not
  an mIRC version number).

Note: `$v1`/`$v2` (the operands of the last comparison — heavily used by real
scripts) are still pending; they need lazy `$iif`/comparison evaluation, since
identifier arguments are currently expanded before the condition runs.

## 26.7.7

mSL engine fidelity: the popup / nicklist identifiers that real mIRC scripts
(like the IRC7 client scripts) lean on, plus file info and ban-event identifiers —
and a help file brought back in sync with what the engine actually does.

### Added (mSL identifiers)
- **`$snick` / `$snicks`** — the nicklist popup selection. `$snicks` is the
  comma-separated list; `$snick(#, N)` is the Nth selected nick (`N=0` → count).
  Threaded from the frontend through the popup run.
- **`$style(N)`** — as a popup item's first word: `1` adds a check mark, `2` greys
  it out (disabled, and a disabled submenu parent won't open), `3` both. Rendered
  in the popup menu.
- **`$submenu($id($1))`** — dynamic menu generation: calls `$id` with `$1` =
  `begin`, then `1, 2, …` until it returns nothing, then `end`, building a flat
  list in place (mIRC semantics; no nesting).
- **`$file(name)`** — file info (`.size`, `.mtime`, `.ctime`, `.atime`, `.name`,
  `.ext`, `.path`, `.attr`), sandboxed to the script-data dir like `$isfile`.
- **`$banmask`** — the full mask set in `on BAN`/`on UNBAN`.
- **`$notags(line)`** — a line with its leading IRCv3 message-tag block removed.

### Fixed
- **`$bnick`** now returns just the *nick part* of a ban mask (`$null` when the
  mask has no real nick, e.g. `*!*@host`), matching mIRC — it was returning the
  whole mask. Use `$banmask` for the full mask.

### Docs
- **Help file** (`/help`): added a **Thanks & credits** section (JD's `ircx-sspi`
  NTLM SSPI, Ricardo's help with the AUTH, Sky's testing); documented the new
  identifiers; and corrected a stale "not supported" list that still claimed
  `$md5`/`$hmac`/`$regsubex`/`$input`/`.ini`/binary vars/DCC/custom `@windows`
  were unavailable when they've long since landed.

## 26.7.6

IRCX **owner/host key** management, born from running jIRC as an owner on a live
IRC7 server: the client now provisions and defends channel ownership itself.

### Added
- **IRCX key provisioning** — on getting `+q`, jIRC generates fresh
  OWNERKEY/HOSTKEY, sets them via `PROP`, grants owner+host `ACCESS` to your
  username mask, and stores the keys per network/channel in `ircx-keys.json`
  in the data folder (human-readable; cached in memory for instant lookup).
- **Owner takeover protection** — if someone else strips your `+q`, jIRC
  reclaims owner with the stored OWNERKEY (`MODE you +h key`), clears the owner
  access list, kicks the offender, and — via the re-triggered provisioning —
  rotates both keys as the final step.
- **`//command` evaluation** — mIRC's "evaluate then run": `//mode $me +h key`
  runs through the mSL engine so identifiers (`$me`, `$chan`, …) evaluate.
  A single `/command` stays literal, exactly like mIRC's editbox.

### Fixed
- **`/mode` target mangling** — an explicit target (`/mode nick +h key`,
  `/mode %#chan +o nick`) no longer gets the active channel prepended; only a
  bare modestring (`/mode +m`) targets the current channel. This broke IRCX
  self-promotion (`MODE <nick> +h <ownerkey|hostkey>`), which the server
  answered with `472`/`482`.

## 26.6.28

Running a real, heavy mIRC script end-to-end — an **IRC7 GateKeeper** socket
load-tester that does GKSSP/HMAC challenge–response auth with byte-level
`$regsubex`/binvar work — surfaced a batch of mSL-engine fidelity bugs. With
these, the script authenticates and joins exactly as it does in mIRC. Guiding
principle: mIRC is the reference; a script that works there but not in jIRC is a
jIRC bug.

### Added
- **`$input`** — mIRC's modal text prompt. `$input(message, type, title, default)`
  now shows the in-app prompt dialog and **blocks until you answer** (the run
  executes on a worker thread so the UI stays responsive), returning the entered
  text, or empty if cancelled. Popup-menu items that gather input (a `Start…`
  flow) work. New `ScriptInput` engine trait + `script-prompt`/`script_prompt_reply`
  wiring; `script/input.rs`.

### Fixed (mSL engine)
- **Empty-value comparisons** — `if (%x == $null)` / `!= $null` always read true
  (`$null` → "" collapsed the tokens). Now correct for single-word values,
  multi-word values (`if (%line != $null)` where `%line` has spaces), and the
  `!$value` truthiness form (which mis-fired when the value held `<`/`=`/`>`).
- **`.command` silent prefix** — `.timer`/`.msg`/`.notice`/… were sent to the
  server as raw lines instead of dispatching; the leading `.` is now stripped.
- **`$asc(" ")`** — identifier arguments were trimmed, so `$asc(" ")` returned
  empty; whitespace-only args are now preserved (byte builders rely on it).
- **`sockwrite name &binvar`** — sent the literal text `&binvar`; now writes the
  binary variable's bytes (needed for binary protocols).
- **`$regsubex`** — three fidelity fixes: a leading `(*UTF8)`/`(*UCP)` PCRE verb
  made the pattern fail (stripped now — Rust's regex is always UTF-8); an
  unrecognised escape such as `\*` dropped its backslash (kept now); and a
  captured group containing mSL-structural chars (`( ) [ ] { } $ % , &`) corrupted
  `$asc(\1)`-style byte builders (now encoded so the value round-trips).
- **Case-insensitive by default** — `$replace`, `$remove`, `$pos`, `$lastpos`,
  `$count`, `$istok`, `$addtok`, `$findtok`, `$remtok`, `$reptok` were
  case-sensitive; mIRC treats them case-insensitively (the `…cs` variants are the
  case-sensitive ones).

### Fixed (display / parsing)
- **CESU-8 / emoji** — astral characters (emoji in nicks) that .NET/IRCX servers
  send as CESU-8 surrogate pairs showed as mojibake; they're now decoded.
- **CTCP ACTION** — an incoming `/me` left a trailing box (the CTCP `\x01` was
  kept by a greedy match); it's stripped now.
- **Popup menus** — the `Label { command }` brace form wasn't parsed (the braces
  leaked into the label, surfacing raw popup code); both `{ }` and `:` command
  forms now parse, with submenu nesting.

## 26.6.27

### Added
- **`/notify`** — maintain a notify (watch) list. `/notify <nick>` adds, `/notify
  -r <nick>` removes, and bare `/notify` prints the list. Stored in settings
  (`notifyList`) and matched case-insensitively.
- **`/ialfill [network] <#channel>`** — populate the IAL by WHOing the channel;
  each WHO reply records that member's address. `/ial`, `/ialclear` and `/ialmark`
  are now recognised as client commands too (no longer mis-sent to the server as a
  raw "421 Unknown command") — mutating the live IAL still needs a
  connection-control channel that isn't built yet.
- **`/links [server]`** — send a LINKS query.
- **`/qmsg <text>`** and **`/qme <action>`** — message (or CTCP ACTION) every open
  query window at once.
- **`/queryrn <oldnick> <newnick>`** — rename an open query buffer (new
  `renameBuffer` store action; a case-only rename keeps the same key).

### Changed
- **`/ignore`** now lists the current ignores when called bare, supports
  `/ignore -r <nick>` to remove an entry, and matches case-insensitively
  (previously it was add-only).

## 26.6.26

### Added
- **Protocol script events** — `on WALLOPS`, `on SNOTICE` (a NOTICE from a
  server), `on ERROR` (a server ERROR message), `on CONNECTFAIL` (a failed
  connect attempt), and `on PING` / `on PONG`. WALLOPS/SNOTICE/ERROR take a
  matchtext; the rest are plain. `$nick` is the sender where applicable, `$1-`
  the message text. Verified against mirc.com.

### Fixed
- **Braceless `on` one-liners** for matchtext-without-target events (`RAW`/
  `WALLOPS`/`SNOTICE`/`ERROR`) and plain events (`CONNECT`/`DISCONNECT`/`PING`/…)
  now parse correctly — the trailing command was previously swallowed as the
  target field, so the handler did nothing.

## 26.6.25

### Added
- **CTCP send** — `/ctcp <nick> <command>` (VERSION, PING, TIME, FINGER,
  USERINFO, CLIENTINFO, SOURCE, …). `/ctcp <nick> ping` reports the round-trip
  latency.
- **`on CTCPREPLY`** script event — fires when a CTCP reply comes back
  (matchtext matches the command word or the full reply).
- **CTCP auto-replies** for **FINGER**, **USERINFO** and **SOURCE**, on top of
  the existing VERSION / PING / TIME; **CLIENTINFO** now advertises them all.
- **DCC file transfer** — send and receive files with progress bars, a nicklist
  **DCC Chat / Send File** menu, and IPv6 support; DCC chat in both directions.
- **Configurable data folder** (Settings → Behaviour) plus a portable-install
  mode; `scripts/`, `dcc/`, `logs/` and friends all live under one `jIRC` folder.

### Changed
- **CTCP TIME** now replies with the **weekday, your local date/time and UTC
  offset** (e.g. `Thu 2026-06-25 14:32:10 +12:00`) instead of UTC — NZST/NZDT in
  New Zealand, the local zone elsewhere, with DST handled automatically. This
  matches mIRC, which replies with your own clock.
- The app data folder was renamed from `com.jirc.app` to **`jIRC`**
  (auto-migrated once at startup).
- Adopted **CalVer** versioning and started this log.

### Fixed
- **`on CTCP` now fires live.** Incoming CTCP requests only produced a status
  echo and never reached the script engine, so `on CTCP` handlers didn't run
  outside unit tests. Requests and replies are now delivered to scripts.

## 0.1.0 — initial

The pre-CalVer baseline: connect with TLS / SASL / IRCX / SOCKS proxy, channels
and queries, the nicklist with a context menu, mIRC-colour rendering with URL
linkifying, the native mSL scripting engine (aliases, `on` events, popups,
identifiers, timers, sockets), per-buffer logging, dark/light/system themes, and
secrets in the OS keyring.
