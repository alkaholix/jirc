# jIRC

A modern, open-source **mIRC-style IRC client** — cross-platform (Windows, macOS,
Linux) and speaking both **standard IRC** (RFC 1459/2812 + some IRCv3) and
**IRCX** (the Microsoft chat extension protocol).

> **Status: usable.** Multi-server chat, TLS/SASL, IRCX, a tabbed/tree UI, a
> channel browser, scriptable popups, and a working mIRC-scripting (mSL) engine.
> The main thing still missing is DCC (file transfer). See [ROADMAP.md](./docs/ROADMAP.md).

## Features

- **Multiple servers at once**, each in its own window; auto-reconnect with backoff
- **Standard IRC + IRCX** — IRCX `IRCX`/`ISIRCX` handshake, `ACCESS`/`PROP`/`LISTX`/
  `WHISPER`; ISUPPORT (`PREFIX`/`CHANTYPES`) so non-standard prefixes work
- **Security & auth** — TLS (rustls), SASL PLAIN, NickServ, SOCKS5 proxy; passwords
  stored in the OS keyring, not in plaintext
- **Chat UI** — collapsible **server tree** *or* **switchbar** (tabs) layout,
  nick list with prefix sorting/colours, full mIRC colour/format rendering,
  clickable URLs, per-buffer logging, desktop notifications & highlight words
- **Detachable windows** — pop any channel, query, or `@window` out into its own
  OS window and dock it back with one click (beyond mIRC's in-app MDI)
- **Channel management** — nick right-click menu (whois/op/voice/kick/ban/ignore),
  topic editing, channel-mode commands, **/list & IRCX /listx channel browser**, and
  an **auto-join channels folder** (per-network add/remove + Join-now)
- **Alternative nickname** with automatic fallback when your nick is in use
- **Ignore list**, CTCP auto-replies (VERSION/PING/TIME), emoji shortcodes
- **Behaviour settings** — rejoin on kick, rejoin after reconnect, skip MOTD,
  ping?/pong! display, raw **trace**, themes (dark/light/system), and more
- **Scripting (mSL)** with **editable popups** — see below

## Scripting (mSL)

A working **subset** of the mIRC scripting language runs natively in the Rust
backend. Edit scripts from the in-app editor (the `⟨⟩` button); multiple `.mrc`
files are compiled together, and an **Examples** button seeds starter scripts.

📖 **[Help &amp; scripting guide (public/help.html)](./public/help.html)** — covers
using the client *and* the full mSL scripting reference, with examples. In the app,
the **?** button opens it in your browser.

- `alias` commands + **custom value-returning aliases** (`/return` → `$myalias`);
  **runtime `/alias`** define/remove
- **Script groups** — `#name on/off … #name end` with `/enable`/`/disable`/`/groups`
  and `$group`; disabled groups' aliases and events don't fire
- `on` event handlers: TEXT/ACTION/NOTICE/**INPUT**/JOIN/PART/QUIT/NICK/**KICK**/
  **MODE**/**TOPIC**/**INVITE**/CONNECT/**DISCONNECT**/**RAW**/**CTCP**/**SIGNAL**,
  plus per-mode **OP/VOICE/BAN/…** events
- **Identity & connect control** — `/anick`/`/mnick`/`/fullname`, and `/autojoin`
  (`-n`/`-s`/`-dN`) to control the connect-time autojoin from `on CONNECT`
- `if`/`elseif`/`else`, `while`, `%variables`, hash tables (with `/hsave`/`/hload`), **`/timer`**
- **Regex** (`$regex`/`$regml`/`$regsub`) and **sandboxed file I/O** (`$read`/`/write`/`$lines`)
- **TCP sockets** (`/sockopen`, `on SOCKREAD`, …) — build sockbots and custom clients
- **Popups**: `menu nicklist { … }` blocks (with submenus) drive the right-click menu
- **Custom dialogs** (`dialog`/`/did`/`$did`/`on DIALOG`) and **custom `@windows`**
  (`/window`/`/aline`/`$line`) — rendered natively; `@windows` detach like any window
- ~55 identifiers (`$me $nick $chan $rand $calc $left/$right/$mid $iif $gettok
  $sorttok $regex $read …`) and commands (`/msg /me /notice /join /mode /set /inc
  /hadd /timer /write …`)

Not 100% mIRC-compatible — DCC (file transfer) is the main remaining gap; see
[ROADMAP.md](./docs/ROADMAP.md) and the [help guide](./public/help.html).

## Install / develop

Prerequisites: [Node.js](https://nodejs.org/) 18+, [Rust](https://rustup.rs/), and
the [Tauri v2 system prerequisites](https://v2.tauri.app/start/prerequisites/).

```bash
npm install          # install frontend deps
npm run tauri:dev    # run the app in development
npm run tauri:build  # produce a release build + installers
npm test             # frontend tests (vitest)
cargo test --manifest-path src-tauri/Cargo.toml -- --skip live   # backend tests
```

## Where your data lives

Stored as JSON (no INI files). Under the app config dir
(Windows: `%APPDATA%/jIRC/`):

```
profiles.json     # saved servers (passwords are in the OS keyring, not here)
scripts/*.mrc     # your scripts, all compiled together
```

Chat logs live under the app data dir as `logs/<network>/<buffer>.log`.
App settings are kept in the webview's local storage.

### Password storage (cross-platform)

**Passwords are stored in the OS keyring**, with a native backend per platform:

- **Windows** → `windows-native` (Credential Manager) ✅ tested
- **macOS** → `apple-native` (Keychain via Security framework) ✅
- **Linux/BSD** → `sync-secret-service` (Secret Service via D-Bus — gnome-keyring/KWallet) ✅
- `crypto-rust` provides the Secret Service session encryption (pure Rust)

Each backend is target-gated, so every OS only pulls its own. If no keyring is
available (e.g. a headless Linux box with no Secret Service daemon), jIRC falls
back to saving the password in `profiles.json` and tells you so in the connect
dialog. On Linux, running needs a Secret Service provider installed
(`gnome-keyring` or `kwallet`).

## Contributing

Architecture, conventions, and build/test details are in [CLAUDE.md](./CLAUDE.md);
the feature matrix and priorities are in [ROADMAP.md](./docs/ROADMAP.md).

## License

[MIT](./LICENSE)
