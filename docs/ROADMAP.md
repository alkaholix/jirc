# jIRC vs. mIRC — Feature Matrix & Roadmap

A high-level view of where jIRC stands against mIRC's feature set.
Status: ✅ done · 🟡 partial · ❌ missing · ⭐ jIRC-only (modern extra)

> For the **granular, tickable checklist** of every mIRC identifier / operator /
> command / event (work through it one by one), see **[PARITY.md](./PARITY.md)**.

> Snapshot as of the current build. jIRC already covers the **daily-driver core**
> (connect, chat, channels, auth) and a **substantial mSL scripting engine**
> (aliases, ~14 events, regex, file I/O, custom identifiers — see
> [public/help.html](./public/help.html)). The big remaining buckets are
> **DCC/file transfer**, **state-aware scripting + dialogs**, and a long tail of
> **power-user tools** (notify list, buffer search, away UI).

---

## 1. Connectivity & protocol

| Feature | Status | Notes |
| --- | --- | --- |
| Multiple simultaneous servers | ✅ | |
| TLS/SSL | ✅ | rustls; insecure-cert toggle |
| SASL | 🟡 | PLAIN only; EXTERNAL/SCRAM missing |
| IRCv3 capabilities | ✅ | message-tags, server-time, away-notify, account-notify, chghost, multi-prefix, extended-join, userhost-in-names; **echo-message** still missing |
| SOCKS proxy | 🟡 | SOCKS5 only (no SOCKS4) |
| IRCX | 🟡 | Handshake + ACCESS/PROP/LISTX/WHISPER; **AUTH packages (GateKeeper/NTLM) not implemented** |
| ISUPPORT (005) | ✅ | PREFIX + CHANTYPES parsed |
| Auto-reconnect | ✅ | exponential backoff |
| On-connect "perform" commands | 🟡 | autojoin only; no arbitrary perform list |
| identd server | ❌ | |
| Secure secret storage | ⭐ | OS keyring (mIRC stores plaintext-ish) |
| Cross-platform (Win/mac/Linux) | ⭐ | mIRC is Windows-only |

## 2. Core chat UI

| Feature | Status | Notes |
| --- | --- | --- |
| Status / channel / query windows | ✅ | |
| Collapsible server tree | ✅ | + switchbar (tabs) layout, switchable in settings |
| Nick list + prefixes | ✅ | |
| mIRC color/format rendering | ✅ | full 99-colour palette |
| Timestamps / logging | ✅ | per-buffer disk logs |
| Topic view + edit | ✅ | |
| Nick right-click menu | ✅ | default + script-editable via `menu nicklist { }` |
| Channel modes UI | 🟡 | via menu/commands; no full mode dialog |
| `/list` channel browser | ✅ | sortable/filterable window; IRCX `/listx` too |
| Clickable URLs | ✅ | open in default browser |
| Search in scrollback | ✅ | Ctrl+F find with match nav |
| Ignore list | ✅ | nick/mask wildcards; settings + /ignore + nick menu |
| Notify/watch list (friends online) | ✅ | ISON polling; alerts + online panel |
| Away system (/away + indicators) | ✅ | toggle + indicator (own away tracked) |
| Highlight/mention rules | ✅ | words + nick |
| Desktop notifications | ✅ | sounds ❌ |
| Color/format input toolbar | ❌ | |
| Spell check | ❌ | |
| Themes (dark/light) | ✅ | font picker ❌ |
| Per-nick colors | ✅ | |
| Detachable (pop-out) windows | ⭐ | any buffer or `@window` detaches to a **real OS window** and docks back in one click — goes beyond mIRC's in-app MDI |
| Free-form in-app docking | 🟡 | tree/switchbar layouts + pop-out windows; no draggable MDI panes |

## 3. DCC & file transfer  (entire bucket ❌)

| Feature | Status |
| --- | --- |
| DCC Chat | ❌ |
| DCC Send / Get (files) | ❌ |
| DCC Resume | ❌ |
| Passive/reverse DCC | ❌ |
| Fserve (file server) | ❌ |

## 4. CTCP

| Feature | Status | Notes |
| --- | --- | --- |
| ACTION (`/me`) | ✅ | |
| VERSION / PING / TIME / CLIENTINFO replies | ✅ | auto-reply to direct requests |
| Custom CTCP | ❌ | |

## 5. Scripting (mSL)

> Full guide with examples: **[public/help.html](./public/help.html)** (also in-app via the **?** button).

**Implemented**
- Aliases ✅ · **custom value-returning aliases** (`/return` + `$myalias`) ✅
- `on` events ✅: TEXT, ACTION, NOTICE, **INPUT** (can `/halt` to suppress the line),
  JOIN, PART, QUIT, NICK, KICK, MODE, TOPIC, INVITE, CONNECT, DISCONNECT, plus
  **per-mode** events **OP/DEOP/VOICE/DEVOICE/OWNER/DEOWNER/ADMIN/DEADMIN/HELP/DEHELP/BAN/UNBAN**
  — events expose their text via `$1-` (kick reason, part/quit msg, topic, etc.)
- `if/elseif/else`, `while`, **`goto`/`:labels`** ✅ · `%variables` ✅ — **`%dotted.names`**
  and **brace-less bodies** (`if (x) cmd`) both supported, like mIRC
- **Condition operators** ✅ `== === != < > <= >= // \\ isin isincs iswm iswmcs isnum
  (incl. range) isletter isalpha isalnum islower isupper`, `&&`/`||`, and `!` negation
- **Named/stoppable timers** ✅ `/timer[name]`, `/timer name off`, `/timers`, `/timers off`
- **Script groups** ✅ `#name on|off … #name end` + `/enable`/`/disable` (wildcards
  `#help*`/`#*`), `/groups [-e|-d]`, `$group` — disabled groups' aliases/events don't fire
- **Runtime `/alias`** ✅ define/replace/remove single-line aliases (persisted to disk)
- **Signals** ✅ `/signal [-n] <name> [params]` + `on SIGNAL` (`$signal`, `$1-`, wildcard names)
- **Identity & connect** ✅ `/anick`/`/mnick`/`/fullname`; `/autojoin` (`-n`/`-s`/`-dN`)
  controls the connect-time autojoin from `on CONNECT`; plus an **auto-join channels
  dialog** (the `#` button) to manage per-network channels with a Join-now action
- **Misc** ✅ `/reload` (recompile scripts), `/unsetall`, `/flushini`/`/saveini`
- **Hash tables** ✅ incl. persistence (`/hmake /hfree /hclear /hadd /hdel /hinc /hdec
  /hsave /hload`, `$hget $hfind`) — `-m`/`-w` switches honoured
- **Sockets** ✅ `/sockopen [-e] /sockwrite /sockread /sockclose`,
  `on SOCKOPEN/SOCKREAD/SOCKCLOSE`, `$sockname $sockbr` — line-based, plain **or TLS** (sockbots work)
- **Regex** ✅ `$regex $regml $regsub`
- **Sandboxed file I/O** ✅ `$read $lines /write $isfile $isdir $exists $scriptdir`
  (confined to a `scriptdata` dir)
- **State-aware identifiers** ✅ `$chan(N) $nick(#,N) $comchan $onchan` (live channel/member
  snapshot) + `$address $mask $ial` (internal address list / host masks)
- Scripted messages echo into your own buffer (like mIRC); IRCX `%` channels resolve `$chan`
- Identifiers (~65): context (`$me $nick $knick $newnick $chan $target $network
  $server`), time (`$time $date $ctime $duration`), strings (`$len $left $right $mid $upper
  $lower $chr $asc $str $reverse $pos $count $replace $remove $strip $qt $+(…)`), numbers
  (`$rand $r $base $round $calc $abs $int $ceil $floor $min $max $iif`), tokens (`$gettok`
  incl. ranges, `$numtok $addtok $istok $findtok $deltok $remtok $puttok $sorttok $wildtok
  $matchtok`), hash (`$hget`)
- Commands: `msg say me describe notice amsg ame query join part nick mode topic ban unban
  quit echo set var unset inc dec tokenize noop hadd hdel hinc hdec hmake hfree hclear
  timer halt return raw write`; unknown **client-side** commands are ignored (not leaked
  to the server as raw)
- **Popups**: `menu nicklist { … }` with submenus, edge-aware positioning
- **Custom dialogs** ✅ `dialog name { … }` (text/edit/editbox/button/check/combo/list),
  `/dialog`, `/did`, `$did`, `on DIALOG` — rendered natively in the web UI (auto-layout)
- **Custom `@windows`** ✅ `/window` (listbox/text), `/aline /iline /rline /dline /clear`,
  `$window` / `$line` — rendered as buffers (1-based line ops) and **detachable** like any window

**Missing (mIRC has these)**

| Area | Status | Notes |
| --- | --- | --- |
| State-aware identifiers | ✅ | `$chan(N) $nick(#,N) $comchan $onchan` + `$address $mask $ial` (internal address list fed by message prefixes / userhost-in-names) |
| Picture / editbox `@windows` | 🟡 | listbox/text `@windows` done (render + poppable); picture/editbox kinds currently render as a text list |
| `on` OPEN/CLOSE/SNOTICE/NOTIFY/START | ❌ | window/notify lifecycle events |
| Property suffixes (`$sock(x).status`, `$hget(t,N).item`, `$chan(#).topic`) | ❌ | base identifiers work; `.property` parsing not yet |
| `$regsubex`, crypto (`$md5 $hmac $sha1`), `$input` dialogs, `.ini` files | ❌ | a long tail of advanced/rarely-used identifiers |
| Binary vars, `$bvar`, DLL/COM | ❌ | DLL/COM intentionally omitted (Windows-only/unsafe) |

## 6. Tools / address book

| Feature | Status |
| --- | --- |
| Server list / favorites | ✅ (profiles) |
| Notify (watch) list | ✅ |
| Ignore list | ✅ |
| URL grabber | ❌ |
| Notes / address book | ❌ |
| Flood protection / auto-op / auto-protect | ❌ |

## 7. Customization & extensibility

> The web-based UI is jIRC's extension surface — the **cross-platform, sandboxed
> answer to mIRC's DCX.dll**. No native DLLs: customization happens through CSS
> theming, user asset packs, and script-driven (HTML-rendered) UI, so anything
> users share stays safe and works on every OS.

| Feature | Status | Notes |
| --- | --- | --- |
| Themes (dark/light/system) | ✅ | |
| Configurable self-nick colour | ✅ | |
| `:shortcode:` emoji | ✅ | built-in set |
| Custom emoji packs (inline images) | ✅ | settings map: text or image-URL `:codes:`, rendered inline |
| Nicklist icons / badges | ✅ | `/nickicon` sets a per-nick emoji/image badge |
| Custom CSS / user themes | ✅ | paste CSS in Settings (with a cheat sheet); applied live |
| Custom dialogs from scripts | ✅ | `dialog`/`/dialog`/`$did`/`on DIALOG`, rendered natively in the web UI |
| Script-driven UI (toolbar buttons, panels) | ❌ | the DCX-equivalent beyond dialogs: scripts add toolbar/panel UI |
| Notification sounds | ❌ | |
| Plugin API (JS / Lua) | ❌ | optional power-user layer (future; not native DLLs) |

---

## Roadmap — suggested priority tiers

> Done since the first cut: ✅ CTCP replies, clickable URLs, `/list` browser,
> ignore list, IRCv3 caps, `$regex`, file I/O, custom identifiers, all the common
> `on` events incl. **per-mode** (OP/DEOP/VOICE/BAN/…) and `on INPUT` with `/halt`,
> **goto/labels**, **named timers**, **sockets** (plain + TLS sockbots),
> **hash persistence**, **state-aware identifiers** (incl. `$ial`/`$address`), and
> **custom dialogs**.

> Also done since: ✅ buffer search, away UI, notify/watch list, custom emoji,
> nicklist icons, **custom `@windows`**, and **detachable (pop-out) windows**.

**Tier 1 — the biggest gap still open**
1. **DCC Chat + Send/Get** — the single biggest "real mIRC" feature still absent.

**Tier 2 — more user customization**
2. **Script-driven UI beyond dialogs** — custom toolbar/menu buttons and panels
   (custom CSS ✅ already done).

**Tier 3 — protocol + polish**
4. **echo-message** (IRCv3), **SASL EXTERNAL/SCRAM**, **IRCX AUTH packages**
   (GateKeeper/NTLM for IRC7-style nets).
5. Window/notify lifecycle script events (`on OPEN/CLOSE/SNOTICE/NOTIFY`).
6. UX: input **color/format toolbar**, **font picker**, notification **sounds**,
   dockable panes, full channel-mode dialog.

**Tier 4 — distribution**
7. CI (build/test on Win/mac/Linux), signed releases, auto-updater, an optional
   **plugin API** (JS/Lua — not native DLLs).

---

### Where jIRC already goes beyond mIRC
Cross-platform, modern UI, OS-keyring secret storage, open-source/MIT, a clean
Rust core with a tested protocol layer.
