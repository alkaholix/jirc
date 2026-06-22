# jIRC — Handoff / Pickup Notes (last updated 2026-06-22)

## TL;DR — the big decision
We **un-pivoted** from the MSL→TypeScript *converter* back to the **native Rust mSL engine**.
Stay **Rust + Tauri + React**. **`jIRC-OLD/` is the version we keep** (it has the native engine);
**`jirc-main/` (the converter version) is being deleted.** Going-forward mSL parity now happens in
**one place** — the Rust engine — instead of a converter + a duplicate TS interpreter.

Full rationale: `C:\Users\John\.claude\plans\reactive-kindling-lemon.md`.

## Folder / repo state — READ FIRST
- `C:\jirc\` currently has: `.git`, `jirc-main\` (**DELETE** — converter), `jIRC-OLD\` (**KEEP** → rename to `jirc`).
- **Git tracks `jirc-main/`**; `jIRC-OLD/` is untracked. After deleting `jirc-main` and renaming `jIRC-OLD`,
  **re-init git** in the kept tree (`git init` fresh — the existing history is the converter's).
- **All this session's work is in `jIRC-OLD/`.** Deleting `jirc-main` loses nothing we need.
- Example/test scripts preserved at **`jIRC-OLD/test-scripts/`** (BV2, Sockbot — used as parity test cases;
  originals also in the OneDrive mIRC backup).
- If a build ever fails with a stale-path / "plugin permissions" error after moving folders:
  **`cargo clean` in `src-tauri`** (that was the only reason jIRC-OLD "didn't build" — a relocation cache, not a bug).

## Stack & where things are
- Tauri v2. Backend `src-tauri/src/`, frontend `src/` (React 18 + TS + Vite + zustand).
- **Native mSL engine: `src-tauri/src/script/`** (`parser`, `ast`, `eval`, `ident`, `mod`, `socket`, `timer`),
  ~5,200 lines, runs `.mrc` directly. **This is where mSL parity work goes.**
- User scripts: `%APPDATA%/com.jirc.app/scripts/*.mrc`.
- Build/test: `cargo test --manifest-path src-tauri/Cargo.toml -- --skip live` · `npm run build` · `npx vitest run` ·
  `npm run tauri build -- --no-bundle` → `src-tauri/target/release/jirc.exe`.

## Verified green
- 92 backend tests, 29 frontend tests pass; release exe builds (last good: 2026-06-22 16:44).

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
1. **Consolidate** (mostly user): delete `jirc-main/`, rename `jIRC-OLD/` → `jirc`, `git init` fresh, light doc
   touch-ups (jIRC-OLD's `CLAUDE.md`/`README.md`/`ROADMAP.md` are already native-first; just note the converter is dropped).
2. **Engine mSL-parity punch-list** — the converter's old gaps, now single-location Rust-engine work.
   Verify each against the native engine first (it may already handle some). Test with `jIRC-OLD/test-scripts/`:
   - **if-then-else operators**: ensure the full table — `== === != < > <= >= isin iswm` **plus** `isincs iswmcs
     isnum isalpha isalnum islower isupper isletter ison isop ishop isvoice isreg ischan isban isnotify isignore
     isaop isavoice isprotect // \\ &` — and **negation** (`!value`, `!op`).
   - **alias params**: `$N-M` ranges, bare `#` = current channel, `$$` require-prefix.
   - **identifiers**: `$address/$site/$fulladdress/$wildsite/$maddress` (engine already has an internal address
     list — verify it covers these), `$event`, `$numeric`.
   - **events**: parenless / mixed `if` (`if ($2==X) && $y==z {…}`), **braceless one-liner `on` events**
     (`on *:TEXT:!cmd:#:/msg …`), CTCP matchtext (match the command **or** full text), `on RAW` (match numeric/command + `$numeric`).
   - **sockets**: confirm `socklisten` reports its bound port for `$sock(name).port`; wildcard `sockclose`/`sockwrite`
     (`name.*`); `$lf`/`$cr`/`$crlf`/`$tab`.
   - (These were catalogued in detail in the now-deleted `jirc-main/docs/scripting-roadmap-checklist.html` — the
     *knowledge* is summarised here; the converter code itself is intentionally gone.)
3. **Optional UI follow-ups**: a Channel Central "bans" view (data already in `useChannelBans`); more Settings
   sections (sounds, address book — each needs supporting code); Toolbar (if you ever want it).
