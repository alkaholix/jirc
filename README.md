# jIRC

A modern, open-source **mIRC-style IRC client** — cross-platform (Windows, macOS,
Linux) and speaking both **standard IRC** (RFC 1459/2812 + IRCv3) and
**IRCX** (the Microsoft chat extension protocol).

> **Status: usable.** Multi-server chat, TLS/SASL, IRCX, IRCv3 capabilities,
> standard and passive DCC, direct DCC Server mode, detachable windows, a
> tabbed/tree UI, and a substantial native **mIRC-scripting (mSL) engine** are
> implemented. Remaining work is mainly power-user UI polish and uncommon
> network/script compatibility variants. See the
> [changelog](./CHANGELOG.md) and the [help &amp; scripting guide](./public/help.html).

## Features

- **Multiple servers at once**, each with its own status/channel/query workspace;
  auto-reconnect with backoff
- **Standard IRC + IRCX** — IRCX `IRCX`/`ISIRCX` handshake, `ACCESS`/`PROP`/`LISTX`/
  `WHISPER`; ISUPPORT (`PREFIX`/`CHANTYPES`/`CHANMODES`/`STATUSMSG`/`CASEMAPPING`)
  so non-standard prefixes and channel types work
- **IRCv3** — server-time, message/account tags, batch, labeled-response,
  chat-history negotiation, away/account/chghost notifications, multi-prefix,
  extended-join, userhost-in-names, and echo-message
- **Security & auth** — TLS (rustls), SASL PLAIN/EXTERNAL/SCRAM-SHA-256/OAUTHBEARER,
  IRCX NTLM/ANON, NickServ, SOCKS5 proxy; passwords and tokens
  normally stored in the OS keyring, with an explicit warned fallback when a
  Linux/BSD Secret Service is unavailable
- **Chat UI** — collapsible **server tree** *or* **switchbar** (tabs) layout,
  nick list with prefix sorting/colours, full mIRC colour/format rendering,
  an optional bold/italic/underline/foreground/background input toolbar,
  clickable URLs, per-buffer logging, desktop notifications & highlight words
- **Detachable windows** — pop any status, channel, query, or `@window` out into its own
  OS window and dock it back with one click (beyond mIRC's in-app MDI)
- **Channel management** — nick right-click menu (whois/op/voice/kick/ban/ignore),
  topic editing, channel-mode commands, **/list & IRCX /listx channel browser**, and
  an **auto-join channels folder** (per-network add/remove + Join-now)
- **Alternative nickname** with automatic fallback when your nick is in use
- **Ignore list**, CTCP auto-replies (VERSION/PING/TIME/FINGER/USERINFO/SOURCE/CLIENTINFO), emoji shortcodes
- **Behaviour settings** — rejoin on kick, rejoin after reconnect, skip MOTD,
  data-folder selection, DCC address/ports/passive mode, DCC Server, ping?/pong!
  display, raw **trace**, themes (dark/light/system), chat font, three timestamp
  modes, custom emoji/CSS, and configurable flood protection
- **DCC** — chat, send/get, resume, passive/reverse transfers, retry/timeout/progress
  UI, sandboxed fileserver, and direct mIRC-compatible DCC Server clients/listener
- **Scripting (mSL)** with **editable popups** — see below

## Scripting (mSL)

A working **subset** of the mIRC scripting language runs natively in the Rust
backend. Edit scripts from the in-app editor (the `⟨⟩` button); multiple `.mrc`
files are compiled together, and an **Examples** button seeds starter scripts.
The editor can pop into a separate resizable window and includes syntax
highlighting, live delimiter diagnostics, draft preservation, and selectable
VS Code Dark+/Light+, Monokai, and Solarized Dark themes.

📖 **[Help &amp; scripting guide (public/help.html)](./public/help.html)** — covers
using the client and the currently implemented mSL surface, with examples. In the app,
the **?** button opens it in your browser.

- `alias` commands + **custom value-returning aliases** (`/return` → `$myalias`);
  runtime `/alias [-l] [file.mrc] name [commands]` define/remove
- **Script groups** — `#name on/off … #name end` with `/enable`/`/disable`/`/groups`
  and `$group`; disabled groups' aliases and events don't fire
- `on` event handlers: TEXT/ACTION/NOTICE/**INPUT**/JOIN/PART/QUIT/NICK/**KICK**/
  **MODE**/**TOPIC**/**INVITE**/CONNECT/**DISCONNECT**/**CONNECTFAIL**/**RAW**/**CTCP**/**CTCPREPLY**/**WALLOPS**/**SNOTICE**/**ERROR**/**PING**/**PONG**/**SIGNAL**/**OPEN**/**CLOSE**/**NOTIFY**/**UNOTIFY**/**START**/**UNLOAD**/**EXIT**,
  socket/UDP, DCC/DCCSERVER, PARSELINE, DIALOG, WEBVIEW, custom-window mouse/listbox,
  per-mode **OP/VOICE/BAN/…** events, and **access-level gating**
- **User access lists** — `/auser`/`/guser`/`/ruser`/`/iuser` with numeric or named
  levels, queried by `$ulist`/`$level`/`$ulevel`/`$clevel`
- **Identity & connect control** — `/anick`/`/mnick`/`/fullname`, and `/autojoin`
  (`-n`/`-s`/`-dN`) to control the connect-time autojoin from `on CONNECT`
- `if`/`elseif`/`else`, `while`, `%variables`, hash tables (with `/hsave`/`/hload`), **`/timer`**
- **Regex** (`$regex`/`$regml`/`$regsub`/`$regsubex`) and **sandboxed file/INI I/O**
- **Sockets** — TCP listeners/clients, UDP, TLS/STARTTLS, queued wildcard writes,
  and byte-exact binary variables
- **Popups**: `menu nicklist { … }` blocks (with submenus) drive the right-click menu
- **Custom dialogs** (`dialog`/`/did`/`$did`/`on DIALOG`) and **custom `@windows`**
  (`/window`/`/aline`/`$line`) with listbox/editbox input, picture canvases,
  drawing/image commands, and mouse events; `@windows` detach like any window
- **Script UI extensions** — toolbar buttons, safe docked text/button panels, and
  isolated managed WebViews with persistent profiles and cookie/navigation events
- **DCC scripting** — chat, transfers, passive/resume handling, sandboxed `/fserve`,
  direct DCC Server commands, identifiers, and pre-accept events with `/halt`
- **200+ identifiers** (`$me $nick $chan $rand $calc $left/$right/$mid $iif $gettok
  $sorttok $regex $read $prop $ulevel …`, plus case-sensitive `…cs` variants) and
  commands (`/msg /me /notice /join /mode /set /inc /hadd /timer /write /auser …`)
- **Faithful mSL evaluation** — `$(...)` (`$eval` short form), **dynamic variables**
  (`%v. [ $+ [ $nick ] ]`), inline `/var` maths, `$prop` for custom identifiers;
  and jIRC evaluates *once*, so other people's text can't turn into commands
  (no mSL-injection footguns — see the help guide's "Safety" section)

Not 100% mIRC-compatible — some protocol and UI depth remains; see the
[changelog](./CHANGELOG.md) and the [help guide](./public/help.html).

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

By default, jIRC keeps its application data in a single **`jIRC` folder under
your profile** (Windows: `%APPDATA%/jIRC/`). Application configuration uses JSON,
not mIRC-style INI files (scripts may still create sandboxed INI data):

```
jIRC/
  profiles.json   # saved servers (secrets normally use the OS keyring)
  scripts/        # your .mrc scripts, all compiled together
  dcc/            # received DCC files
  logs/           # chat logs, <network>/<buffer>.log
  scriptdata/     # sandbox for script file I/O ($read / /write)
```

**Custom / portable location.** To store data elsewhere, pick a folder in
**Settings → Behaviour → Data folder** (applies on restart). For an unattended
or portable setup you can instead set the `JIRC_DATA_DIR` environment variable,
or drop a `portable.txt` file next to the executable (then everything lives in a
`data/` folder beside the app). App settings are kept in the webview's local
storage. *(On Linux, the default `logs/` follow the OS data dir; a custom
location keeps them together.)*

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

The [changelog](./CHANGELOG.md) tracks what's landed and the
[help &amp; scripting guide](./public/help.html) documents the supported client
and scripting surface. Build and
test with the commands above; the IRC/IRCX protocol logic lives in
`src-tauri/src/irc/` and the mSL engine in `src-tauri/src/script/`.

## License

[MIT](./LICENSE)
