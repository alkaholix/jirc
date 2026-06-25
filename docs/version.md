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
