# jIRC — Handoff / Pickup Notes (last updated 2026-06-22)

## TL;DR — the big decision
We **un-pivoted** from the MSL→TypeScript *converter* back to the **native Rust mSL engine**.
Stay **Rust + Tauri + React**. **`jIRC-OLD/` is the version we keep** (it has the native engine);
**`jirc-main/` (the converter version) is being deleted.** Going-forward mSL parity now happens in
**one place** — the Rust engine — instead of a converter + a duplicate TS interpreter.

Full rationale: `C:\Users\John\.claude\plans\reactive-kindling-lemon.md`.

## Folder / repo state — READ FIRST
- **Consolidation is DONE.** The native engine lives at the **`C:\jirc` repo root** (no more `jirc-main/` or
  `jIRC-OLD/`). Git history was **re-init'd fresh** — initial commit `fdab73d`, then one commit per punch-list item.
- Remote `origin` (github.com/alkaholix/jirc) is re-added but **NOT pushed** — needs a force-push (your call).
- Old converter history is archived **outside the repo** at `~/jirc-history-backup.bundle` (`C:\Users\John\…`).
- Example/test scripts at **`test-scripts/`** (BV2, Sockbot — parity test cases; originals also in the OneDrive
  mIRC backup). NB: the BV2 v0.31 sockbot exercises listening sockets (see TODO).
- Build gotcha: stale `jIRC-OLD\…` paths can linger in `target/` after a move — `cargo clean` (or wipe
  `target/debug`) fixes it. The running release `jirc.exe` locks `target/release`, so don't `cargo clean` that
  while the app is open.

## Stack & where things are
- Tauri v2. Backend `src-tauri/src/`, frontend `src/` (React 18 + TS + Vite + zustand).
- **Native mSL engine: `src-tauri/src/script/`** (`parser`, `ast`, `eval`, `ident`, `mod`, `socket`, `timer`),
  ~5,200 lines, runs `.mrc` directly. **This is where mSL parity work goes.**
- User scripts: `%APPDATA%/com.jirc.app/scripts/*.mrc`.
- Build/test: `cargo test --manifest-path src-tauri/Cargo.toml -- --skip live` · `npm run build` · `npx vitest run` ·
  `npm run tauri build -- --no-bundle` → `src-tauri/target/release/jirc.exe`.

## Verified green
- 106 backend tests, 29 frontend tests pass; `cargo check` + full debug build clean.
- Build gotcha learned: a `SocketManager::rename` self-deadlock (re-locking a Mutex through an
  `if let` guard) hung `cargo test` — looked like an "environment hang". If a test hangs, suspect
  a double-lock, and check `Get-Process cargo,rustc` CPU (idle = deadlocked, not compiling).
- Listening-socket async accept/connect I/O still needs a **live-network** test (relay sockbot).

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
1. **Push when ready:** the fresh history isn't on GitHub yet — `git push -f origin main` (remote re-added,
   not pushed). Optional: re-run the release build with the app closed (`npm run tauri build -- --no-bundle`).
2. **Live-test listening sockets** — implemented this session (`/socklisten`/`/sockaccept`/`/sockmark`,
   `on SOCKLISTEN`, `$sock(name).port/.mark/.status`); the *synchronous* bind/port/query path is unit-tested, but
   the **async accept/connect I/O needs verifying on a live network** (e.g. the BV2 v0.31 relay sockbot). Design:
   `/socklisten` binds synchronously via the `ScriptSockets` handle on the engine (so `$sock().port` reads inline);
   `Action::SockListen` starts the accept loop at apply-time with the owning connection's `server_id`. Known gaps:
   `/sockmark` on a *bound-but-not-yet-started* listener is a no-op; `$sock().wsmsg`/`$sockerr` detail is minimal.
3. **Optional UI follow-ups**: a Channel Central "bans" view (ban data now in the state snapshot); more Settings
   sections (sounds, address book); Toolbar.

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
