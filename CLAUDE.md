# CLAUDE.md — jIRC project guide

Guidance for AI assistants and contributors working in this repo. Keep it current.

## Working rules (read first)

1. **Do only what was asked.** Implement the requested change and nothing more.
   No speculative features, helpers, refactors, dependencies, config, or "nice to
   haves" that weren't requested. If something extra seems genuinely needed, ask
   first or call it out — don't just add it.
2. **Don't break existing code.** Changes must be backward-compatible unless the
   user asked otherwise. Before editing shared code (a `UiEvent`, a command
   signature, a store action, a struct field), find every call site and update or
   preserve them. Prefer the smallest, most local change that works.
3. **Verify before claiming done.** Build and run the tests for whatever you
   touched — backend (`cargo test … -- --skip live`) and/or frontend (`npm test`,
   `npm run build`). If you changed wiring (commands, events, plugins), do a
   release build. Never report something as working that you haven't checked.
4. **Match the surrounding code.** Follow existing patterns, naming, and structure
   (see Conventions below). Don't introduce new styles or abstractions casually.
5. **When in doubt, keep it simple and fast.** This project prefers minimal,
   readable code over cleverness or premature generalization.

## What this is

**jIRC** — a modern, cross-platform, open-source (MIT) IRC client aiming at mIRC's
feature set. Speaks standard IRC (RFC 1459/2812 + some IRCv3) and **IRCX**.

- **Shell:** Tauri v2 (Rust backend + web frontend)
- **Backend:** Rust + tokio; protocol on `irc-proto`
- **Frontend:** React 18 + TypeScript + Vite, `zustand` state
- **Philosophy:** fast and simple. Prefer JSON over INI, avoid heavy abstractions,
  keep the protocol logic pure and unit-tested.

## Commands

```bash
npm install                       # frontend deps (first time)
npm run tauri:dev                 # run the app (dev)
npm run build                     # type-check + build frontend
npm test                          # frontend tests (vitest)
cargo test --manifest-path src-tauri/Cargo.toml -- --skip live   # backend tests
npm run tauri build -- --no-bundle    # release build (validates full integration)
```

Live network tests are `#[ignore]`d:
`cargo test --manifest-path src-tauri/Cargo.toml -- --ignored live_libera`.

## Backend layout (`src-tauri/src/`)

- `lib.rs` — Tauri builder: plugins, managed state (`ConnectionManager`,
  `ScriptEngine`), `invoke_handler`, startup `setup` (loads scripts).
- `commands.rs` — `#[tauri::command]` IRC actions (connect/join/msg/whois/…).
- `config.rs` — `ServerProfile` (+ `Proxy`); serde `camelCase`.
- `storage.rs` — profiles JSON + per-buffer logs; secrets in the OS keyring.
- `irc/`
  - `manager.rs` — `ConnectionManager`: owns connections, spawns `supervise`.
  - `connection.rs` — `supervise` (reconnect loop) → `run_once` (one connection,
    `tokio::select!` over socket read + outgoing channel). **`process_message` is
    pure** (msg + state → outgoing lines + `UiEvent`s); test it directly.
  - `event.rs` — `UiEvent` enum emitted to the frontend on the `irc-event` channel.
  - `state.rs` — `SessionState` + `Isupport` (PREFIX/CHANTYPES).
  - `stream.rs` — `NetStream` (plain/TLS via rustls-ring) + SOCKS5.
  - `auth.rs` — CAP + SASL PLAIN.
  - `ircx.rs` — IRCX numerics (800–999) and commands.
- `script/` — the mSL engine (`parser`, `ast`, `eval`, `ident`, `mod`). Pure;
  produces `Action`s applied by `apply_actions`.

## Frontend layout (`src/`)

- `lib/api.ts` — typed wrappers over `invoke` + the `IrcEvent` union (mirrors `UiEvent`).
- `state/store.ts` — zustand store: buffers (status/channel/query), event routing.
- `state/settings.ts` — settings (localStorage).
- `ircFormat/parse.tsx` — mIRC colour/format → React, URL linkify.
- `lib/slash.ts` — `/command` handling. `lib/emoji.ts` — `:shortcode:` expansion.
- `components/` — Sidebar (tree) / SwitchBar (tabs), MessageList (virtualized),
  NickList (+ context menu), TopicBar, InputBar, dialogs (Connect/Settings/Script).

## Data / folder structure (on the user's machine)

Everything lives under a single **`jIRC` folder** (Windows: `%APPDATA%/jIRC/`).
`storage.rs` resolves the base via `config_dir(app)` — all other helpers
(`scripts_dir`, `dcc_dir`, `logs_dir`, `script_data_dir`) hang off it:

```
jIRC/
  profiles.json   # server profiles (NO passwords — those are in the OS keyring)
  scripts/        # mSL script files, all compiled together (main.mrc, <name>.mrc)
  dcc/            # received DCC files
  logs/           # chat logs, <network>/<buffer>.log
  scriptdata/     # sandbox for $read / /write
```

**Path resolution (`storage.rs`):** the base is, in priority — the
`JIRC_DATA_DIR` env var; else a `data/` folder next to the exe when a
`portable.txt` marker sits beside it (portable install); else
`<os config dir>/jIRC` (default, under the profile, name = `APP_DIR_NAME`, not
the bundle identifier `com.jirc.app`). `migrate_legacy_app_dir` renames an old
`com.jirc.app/` folder to `jIRC/` once at startup (skipped for custom bases).
`config_dir`/`app_data_dir` differ only in the **default** case on Linux (OS
config vs data dir); a custom base unifies them.

**Settings** live in the webview's `localStorage` (`jirc.settings`) — chosen for
speed/simplicity (sync, no startup flash). Secrets use the **OS keyring** (service
`jirc`). **No INI files** — JSON everywhere.

## Conventions & gotchas

- **`UiEvent` serialization:** the enum uses
  `#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]`.
  Both attrs are required — `rename_all` alone does NOT camelCase variant *fields*
  (this caused an early "events never reached the UI" bug). There's a test guarding it.
- **TLS:** rustls is pinned to the **ring** provider (no aws-lc-rs C/NASM build dep);
  the provider is installed in `lib.rs::run`.
- **keyring** is v3 (v4 pulls a heavy turso/roaring backend needing newer rustc).
  Features `apple-native` (macOS Keychain), `windows-native` (Credential
  Manager), `sync-secret-service` + `crypto-rust` (Linux/BSD Secret Service via
  D-Bus) — each backend is target-gated, so every OS only pulls its own. Linux
  builds/runs need a Secret Service provider (gnome-keyring/KWallet) and libdbus;
  `storage::keyring_available()` probes it and the connect dialog falls back to
  saving the password in `profiles.json` when it's missing.
- **Adding a command:** write it in `commands.rs`/`script`/`storage`, register it in
  `lib.rs` `generate_handler!`, and add a typed wrapper in `lib/api.ts`.
- **Adding a `UiEvent`:** add the variant (camelCase fields), handle it in
  `store.ts handleEvent`, and add it to the `IrcEvent` union in `api.ts`.
- **Detachable windows (pop-out):** a popped-out buffer/`@window` opens as its own
  `WebviewWindow` via the `open_detached_window` command — which **must be `async`**.
  A *sync* command runs on the main/event-loop thread, so calling
  `WebviewWindowBuilder::build()` there deadlocks WebView2's init and you get a blank,
  unresponsive window (the frame appears but the page never loads — not even an inline
  `<script>` in `index.html` runs). Other gotchas learned here: load `index.html`
  **cleanly** (a URL `#fragment` is treated as part of the asset path and 404s in
  release builds); the detached window finds which buffer to show from its **own window
  label** (`detached-*`) via shared same-origin `localStorage`, not a URL route; and it
  renders with `className="app detached"`, which needs the `.app.detached` flex rule
  (it has no `layout-*` class to provide `display`). See `docs/FLOATING-WINDOWS-DESIGN.md`.
- Tests: protocol logic in `process_message`/`script` is pure — prefer unit tests
  there over end-to-end. Verify the real app by building + launching when feasible.

## Status & roadmap

See `docs/ROADMAP.md` for the mIRC feature matrix and priorities. DCC, deep scripting
(state-aware identifiers, more `on` events, dialogs), and IRCv3 are the big open areas.
