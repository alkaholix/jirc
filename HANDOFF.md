# jIRC — Handoff / Pickup Notes (last updated 2026-06-23)

## TL;DR — the big decision
We **un-pivoted** from the MSL→TypeScript *converter* back to the **native Rust mSL engine**.
Stay **Rust + Tauri + React**. **`jIRC-OLD/` is the version we keep** (it has the native engine);
**`jirc-main/` (the converter version) is being deleted.** Going-forward mSL parity now happens in
**one place** — the Rust engine — instead of a converter + a duplicate TS interpreter.

Full rationale: `C:\Users\John\.claude\plans\reactive-kindling-lemon.md`.

## Folder / repo state — READ FIRST
- **Consolidation is DONE.** The native engine lives at the **`C:\jirc` repo root** (no more `jirc-main/` or
  `jIRC-OLD/`). Git history was **re-init'd fresh** — initial commit `fdab73d`, then one commit per punch-list item.
- Remote `origin` (github.com/alkaholix/jirc) is **pushed** (force-pushed 2026-06-23; the fresh history replaced the
  old diverged remote). The old remote head is preserved locally as tag `archive-old-origin-20260623`.
- Old converter history is archived **outside the repo** at `~/jirc-history-backup.bundle` (`C:\Users\John\…`).
- Example/test scripts (`test-scripts/`, BV2/Sockbot) are now **gitignored + untracked** (local-only; they kept
  reappearing because they had been committed). The BV2 v0.31 sockbot exercises listening sockets (see TODO).
- Build gotcha: stale `jIRC-OLD\…` paths can linger in `target/` after a move — `cargo clean` (or wipe
  `target/debug`) fixes it. The running release `jirc.exe` locks `target/release`, so don't `cargo clean` that
  while the app is open.

## Stack & where things are
- Tauri v2. Backend `src-tauri/src/`, frontend `src/` (React 18 + TS + Vite + zustand).
- **Native mSL engine: `src-tauri/src/script/`** (`parser`, `ast`, `eval`, `ident`, `mod`, `socket`, `timer`,
  `files`, `ini`), runs `.mrc` directly. **This is where mSL parity work goes.**
- User scripts: `%APPDATA%/com.jirc.app/scripts/*.mrc`.
- Build/test: `cargo test --manifest-path src-tauri/Cargo.toml -- --skip live` · `npm run build` · `npx vitest run` ·
  `npm run tauri build -- --no-bundle` → `src-tauri/target/release/jirc.exe`.

## Verified green
- 118 backend tests, 29 frontend tests pass; `cargo check` + full debug + release build clean.
- Build gotcha learned: a `SocketManager::rename` self-deadlock (re-locking a Mutex through an
  `if let` guard) hung `cargo test` — looked like an "environment hang". If a test hangs, suspect
  a double-lock, and check `Get-Process cargo,rustc` CPU (idle = deadlocked, not compiling).
- Listening-socket async accept/connect I/O still needs a **live-network** test (relay sockbot).

## Done 2026-06-23 (autonomous PARITY run — 118 tests green, PARITY 204→285 done)
Worked through `PARITY.md` in batches, reading the mirc.com per-topic help page before each. One commit per
batch (+ a PARITY checkbox commit); build green at every step. New identifiers/commands all have unit/e2e tests.
- **File-handle I/O** (new `script/files.rs`): `/fopen /fwrite /fclose /fseek` + `$fopen/$fread/$fgetc/$feof/$ferr`.
  `FileStore` persists in engine global state like hash tables; re-opens per op; sandbox-confined. `/fseek` does
  byte/`-l`line/`-n`next/`-p`prev/`-w`wildcard/`-r`regex.
- **Math/trig:** `$sqrt $cbrt $hypot`, `$sin/$cos/$tan` (+ hyperbolic + inverse, `.deg` property), `$log/$log2/$log10`, `$pi`.
- **Hashing:** `$md5 $sha1 $sha256 $sha384 $sha512 $crc` — added md-5/sha1/sha2/crc32fast (pure-Rust, no C deps).
- **Bitwise/int:** `$and $or $xor $not $biton $bitoff $isbit $gcd $lcm`.
- **Misc identifiers:** `$day $ord $longip $os`; `$prefix $chanmodes $chantypes` (added ISUPPORT to `StateSnapshot`);
  `$replacex $powmod $utfencode $utfdecode $ticksqpc`; `$encode/$decode` (base64 `m` + percent `x`). Local-time via chrono (06-22).
- **Commands:** protocol `/kick /invite /hop /knock /away /omsg /onotice /ctcpreply /nickserv|/chanserv|/memoserv`
  (the generic raw fallback got trailing-text `:` wrong); sandboxed file `/mkdir /rmdir /copy /rename /remove`.
- **File/path identifiers:** `$findfile`/`$finddir(dir,wild,N[,depth])` (sandboxed recursive walk, N=0 = count),
  `$tempfn`, `$mircexe`.
- **Events:** `on RAWMODE` (channel, raw `$1-`) and `on USERMODE` (your own user mode) in `drive_event`'s Mode branch.
- **More identifiers:** `$isalias $rands $modinv $mircpid`; connection/self-state `$port $ssl $anick $fullname $usermode $away`
  — added connection facts to `SessionState`→`StateSnapshot` + `user_mode`/`away` tracking in `process_message`, so the
  snapshot now carries them (this is the pattern for the rest of the connection/self-state tail).
- **Deferred (deliberate):** `$hash` (mIRC private algo), `$maxlenl/m/s`/`$ip` (need exact values), `$eval`/`$v1`/`$v2`
  (engine pre-expands identifier args), crypto-auth `$hmac/$totp/$hotp/$crc64` (exact signatures/vectors not pinned down online).

### Remaining PARITY (≈427 open) — what each bucket needs
Most open items are **subsystem-blocked**, not quick wins:
- **Custom windows / display** (`/window`, `$window`, `$line`, `aline/rline/dline/cline/...`, `/draw*`): a custom-`@window` subsystem.
- **DCC** (`/dcc *`, `$dcc*`, `on FILERCVD/SENDFAIL/CHAT/...`): a DCC subsystem (file transfer + chat).
- **Dialogs** (`$dialog`, `/dialog`, `$did*` beyond `$did`): a custom-dialog GUI.
- **COM / DDE / DLL / agent** (`$com*`, `$dde*`, `$dll*`, `$agent*`, `/g*`): FFI/COM/automation — likely *(skip)* off-Windows.
- **Media** (`$sound/$play/$insong/$inwave/...`, `/splay`): audio playback.
- **Client/connection state:** `$port/$ssl/$anick/$fullname/$usermode/$away` are **done** (via `SessionState`→`StateSnapshot`,
  which now carries connection facts + user_mode/away). Open by the **same snapshot pattern**: `$serverip` (needs the resolved
  IP), `$awaymsg/$awaytime`, `$idle`, `$online`. GUI/window-state ones (`$active`, `$mouse`, ...) still need the window subsystem.
- **Feasible next (no big subsystem):** crypto `$hmac/$totp/$hotp/$pbkdf2/$argon2/$crc64` (add hmac/pbkdf2/argon2 crates — needs
  the help file's exact param order + test vectors to match mIRC); the binary-var family (`/bset/$bvar/&binvar/...`); the
  connection/self-state tail above via the now-established snapshot pattern.

## Done 2026-06-22 (mirc.com docs audit — parity round, 109 tests green)
Read the official per-topic mirc.com help pages first, then implemented/fixed against them. All committed.
Granular tracking lives in **`PARITY.md`** (items checked off as completed).
- **Sockets — finished the listening subsystem:** `/socklisten` (synchronous bind so inline `$sock().port` works),
  `/sockaccept`, `/sockmark`, `/socklist`, `/sockrename`, `/sockpause`, `on SOCKLISTEN`, `$sock()` props, wildcard
  `/sockwrite`. (Async accept/connect I/O still wants a live-net test — see TODO 2.)
- **Local time via `chrono`:** mIRC uses LOCAL time; ours was UTC. `$time`, `$date` (dd/mm/yyyy), `$fulldate`,
  `$asctime([N,]fmt)` (mIRC→chrono format translator), `$timezone`; `$daylight`=0. Added `chrono` as a **direct**
  dep — it was already transitive (clock feature on), so nothing new compiles.
- **INI is real now:** `/writeini` + `/remini` were no-op stubs → backed by `script/ini.rs` (ordered
  `[section]`/`item=value` parser, case-insensitive). `$readini(file,[n],section,item)`, `$ini` enumeration.
  Sandbox-confined like `/write`/`$read`.
- **String/file/number identifiers:** multi-arg `$pos`/`$lastpos`/`$mid`(N=0=to-end)/`$count`/`$replace`/`$remove`/
  `$instok`/`$reptok`; `$regex`/`$regsub` `/pattern/flags`; `$nopath`/`$nofile`/`$longfn`/`$shortfn`/`$noqt`/
  `$envvar`/`$bytes`/`$gmt`/`$ticks`.
- **Skipped (architecture):** `$eval` / `$regsub %var` (identifier args are pre-expanded), `$v1`/`$v2`, `$hash`
  (algorithm unknown).

## Done this session — ported onto jIRC-OLD (all green)
- **Local console** — `App.tsx` (`openLocalConsole` + welcome "Open a local console"). Run scripts/sockbots with no connection.
- **Channel Central** (`/channel`) + **channel mode & ban tracking** — `state/channelModes.ts`
  (`useChannelModes`/`useChannelBans`/`useChannelCentral`/`routeModeEvent`), `components/ChannelCentral.tsx`, wired in `App.tsx`.
- **Multi-context popups with engine label-eval** (the meaty one):
  - Backend `script/mod.rs`: `popups_evaluated()` + `eval_popup_labels()` evaluate dynamic `$iif`/`$sock` labels
    and drop empty items (mIRC behaviour); `script_popups` command now takes `(serverId, target, myNick, network, context, nick)`.
  - Frontend: `components/popupMenu.tsx` (ContextMenu/PopupItems/SubMenu); NickList (nicklist, `$1`=nick);
    MessageList (channel/query/status right-click → `api.scriptPopups` → `api.scriptRunPopup`).
- **URL grabber** (`/urls`) — `state/urlGrabber.ts` + `routeUrlEvent` in `App.tsx`.
- **Settings**: chat font + size (`applyChatFont`, CSS var on `.line`) + default quit message — `state/settings.ts`,
  `App.tsx` effect, `SettingsDialog` inputs, `/quit` uses `quitMessage`.
- **Commands**: `/dns`, `/url`, `/exit`, `/partall` + backend `open_url`/`exit_app`/`dns_lookup`
  (`commands.rs` + `lib.rs` + `api.ts` + `slash.ts`).
- **Skipped**: Toolbar (redundant with OLD's sidebar actions).

## How scripting integrates (pickup context)
- Typed input: `src/lib/slash.ts` handles core client commands; `default` → `api.scriptRunCommand(...)` →
  engine (script commands → user aliases → raw IRC fallback).
- Engine (`script/mod.rs` `ScriptEngine`): `load`, `has_alias`, `popups`/`popups_evaluated`, `run_alias`,
  `run_command`, `dispatch_event`; produces `Action`s applied by `apply_actions`. `Runtime::expand(text)` evaluates `$identifiers`.
- Frontend `api.script*`: `scriptRunInput` (on INPUT), `scriptRunCommand`, `scriptRunAlias`, `scriptPopups`,
  `scriptRunPopup`, `scriptRunDialog`, `scriptsList/Read/Write/Delete`.

## TODO — next
1. **Push:** done (force-pushed to GitHub 2026-06-23). Going forward a normal `git push` works (no more divergence).
2. **Live-test listening sockets** — implemented this session (`/socklisten`/`/sockaccept`/`/sockmark`,
   `on SOCKLISTEN`, `$sock(name).port/.mark/.status`); the *synchronous* bind/port/query path is unit-tested, but
   the **async accept/connect I/O needs verifying on a live network** (e.g. the BV2 v0.31 relay sockbot). Design:
   `/socklisten` binds synchronously via the `ScriptSockets` handle on the engine (so `$sock().port` reads inline);
   `Action::SockListen` starts the accept loop at apply-time with the owning connection's `server_id`. Known gaps:
   `/sockmark` on a *bound-but-not-yet-started* listener is a no-op; `$sock().wsmsg`/`$sockerr` detail is minimal.
3. **Optional UI follow-ups**: a Channel Central "bans" view (ban data now in the state snapshot); more Settings
   sections (sounds, address book); Toolbar.
4. **Next mSL-parity work:** file-handle I/O is **done** (2026-06-23). See the "Remaining PARITY" map under
   *Done 2026-06-23* for what's left and what each bucket needs — the feasible next items (no big subsystem) are
   crypto (`$hmac/$totp/$hotp/$pbkdf2/$argon2/$crc64`), the binary-var family, and `$encode/$decode`. The big
   remaining areas (custom windows, DCC, dialogs, COM/DDE) each need a dedicated subsystem and your design input.

### Done this session (mSL-parity punch-list — all committed + tested, 102 backend tests)
- **Identifiers:** `$crlf $cr $lf $tab`; event address `$address` (bare) `/$site/$fulladdress/$wildsite`;
  `$event`/`$numeric`.
- **if-then-else:** list operators `ison isop ishop isvoice isowner isadmin isreg ischan` + `isban` (with live
  `+b`/`-b` and RPL_BANLIST 367 ban tracking) + `&`; no-space and mixed-paren conditions (`if ($2==X) && $y==z`).
- **Alias params:** `$N-M` ranges, bare `#` = current channel, `$$` require-prefix.
- **Events:** braceless one-liner `on`; `on CTCP` (matchtext = command OR full text); `on RAW`.
- **Sockets:** wildcard `/sockwrite` (`/sockclose` already fanned out).
- **Skipped (deliberate):** `$maddress` (not a standard mIRC identifier); list operators `isnotify isignore isaop
  isavoice isprotect` (no client-side notify/ignore/userlist subsystem to back them).
- **Note:** `$$N`-empty halts the *rest* of the run but doesn't suppress the current command mid-flight.
