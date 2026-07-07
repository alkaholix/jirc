# CLAUDE-OPUS-4.8.md — how to work on jIRC like Fable 5

You are Claude Opus 4.8 working on **jIRC**. This file is a working manual written
by Fable 5 so you inherit the same instincts, cadence, and discipline. Read it
alongside **[CLAUDE.md](./CLAUDE.md)** (architecture + working rules) — that file is
authoritative for *what the project is*; this file is *how to move through it*.

If you internalise nothing else: **mIRC is the reference, verify before you claim,
test everything you touch, and ship in small versioned increments.**

---

## 0. Prime directives (in priority order)

1. **Fidelity to mIRC, proven from the source.** The behaviour spec lives in the
   compiled help file `C:\i7\mIRC\mirc.chm`. Decompile it once per session
   (`hh.exe -decompile <outdir> "C:\i7\mIRC\mirc.chm"`) and grep the HTML. **Never
   implement an identifier/command from memory or a guess.** If the exact
   semantics (e.g. an ANSI→mIRC colour table, a hash algorithm) aren't in the
   CHM, do **not** ship a mIRC-labelled feature that guesses them — pick a
   different item. A subtly-wrong `$foo` is worse than an absent one.
2. **Verify before "done".** No change is finished until the relevant tests and
   builds pass and you've *seen* the green. See §2.
3. **Do only what was asked; keep it simple.** Match surrounding patterns. The
   smallest, most local change that works. No speculative helpers or refactors.
   (This is CLAUDE.md rule #1 — it dominates your Opus instinct to generalise.)
4. **Small, shippable increments.** One coherent feature → test → version bump →
   changelog → commit → push. Don't accumulate a giant uncommitted pile.
5. **Backward compatible.** Before editing shared code (a `UiEvent`, a command
   signature, `RunCtx`/`Runtime`, an `Action`, a store action), find *every*
   construction/call site and update them all. The compiler's "missing field"
   errors are your checklist — chase them to zero.

---

## 1. The loop for every change

```
understand → confirm semantics from mirc.chm → find the gap in the engine
    → implement minimally, matching patterns → unit-test the pure logic
    → build (backend + frontend) → release build if you touched wiring
    → tick docs/PARITY.md → update public/help.html → bump version + changelog
    → commit → push
```

Start each feature by **grounding in a real script**: `C:\mIRCmodrn\scripts\i7.mrc`
is the exemplar IRC7 client. Diff its identifier/command usage against the engine
to find genuine gaps rather than implementing things nothing uses:

```bash
grep -oE '\$[a-zA-Z][a-zA-Z0-9]*' i7.mrc | sort -u   # its identifiers
# then check each against src-tauri/src/script/ident.rs + eval.rs
```

`docs/PARITY.md` is the full mIRC index as a tickable backlog — use it as the
worklist, and correct any wrong checkmarks you find. Note that a `$submenu`-style
"miss" can be a false positive (it's handled in the popup path in `mod.rs`, not
`ident.rs`), so confirm before declaring something missing.

---

## 2. Verification gates (never skip, never fake)

```bash
# Backend — pure logic, fast. Run for anything under src-tauri/src.
cargo test --manifest-path C:\jirc\src-tauri\Cargo.toml -- --skip live

# Frontend — run if you touched anything under src/
npm test          # vitest
npm run build     # tsc + vite (catches TS + wiring type errors)

# Release integration build — REQUIRED when you changed wiring
# (a #[tauri::command], generate_handler!, an event, a plugin, a struct the
#  command layer serialises). Validates the whole thing links.
npm run tauri build -- --no-bundle
```

- A running `jirc.exe` **locks** `target/release/jirc.exe`; the release build then
  fails with exit 1 / "Access is denied" *after compiling fine*. That's benign —
  the compile is what matters. Stop the stale instance and rerun if you need the
  binary: `Get-Process jirc | Stop-Process -Force`.
- Watch the test **count go up** — a new feature adds tests. If the count is flat,
  your test didn't register.
- The harness surfaces `rust-analyzer` diagnostics that **lag one edit behind** and
  that **falsely flag `#[tauri::command]` functions as dead code** (it can't see the
  `generate_handler!` macro). Trust `cargo test`/`cargo build`, not the lag.

---

## 3. The release ritual (CalVer)

Version = release date `YY.M.D`, but in practice **the last field increments once
per release** (26.7.9 → 26.7.10 → 26.7.11…), so a same-day second release just
bumps it. Set the **same** number in **four** places, then changelog:

- `package.json`
- `src-tauri/Cargo.toml`
- `src-tauri/tauri.conf.json`
- `src-tauri/Cargo.lock` (the `name = "jirc"` package entry)
- add a top entry to `docs/version.md` (newest first; `### Added` / `### Fixed`
  sections; explain the *why*, and note anything still pending)

Commit message: first line `YY.M.D — <headline>`, then a terse body listing what
landed and the "Verified: N backend tests, frontend build, release build clean"
line. **End every commit with** `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`
(keep this line — it's how the project attributes AI commits; adjust the name to
yourself if the user asks). Branch off `main` only if asked; the user has been
committing straight to `main` and pushing. **Only commit/push when the user asked**
you to, or when they've said "keep shipping" — otherwise leave it staged and say so.

---

## 4. mIRC fidelity rules

- **Read the CHM entry in full**, including the Note lines — they carry edge cases
  (e.g. `$snick(#)` with no N returns all selected nicks; `$bnick` is `$null` when
  the ban mask has no real nick; `$active` "can return `$null`").
- **Match exact return shapes**: `$snicks` is comma-separated; `$scid(-1)` is the
  active cid; `$scon(0)` is a count; empty/`$null` where mIRC returns nothing.
- **Fix latent infidelities you find** even if not asked — but only with a test and
  a changelog note (e.g. `$bnick` was returning the whole mask; corrected to the
  nick part, added `$banmask` for the full mask, updated the guarding test).
- When jIRC has **no analogue** for an mIRC concept (numeric window/connection ids
  originally; the leave-menu `$window`), you *build the missing model* (a registry)
  rather than stubbing — see §6.

---

## 5. Architecture cheat-sheet — how context reaches a script

The mSL engine (`src-tauri/src/script/`) is pure and unit-tested. Per run it sees:

- **`RunCtx`** (per-invocation, built by the Tauri command handlers / connection
  layer): `my_nick`, `network`, `server`, `data_dir`, and a `state: Arc<StateSnapshot>`.
  ~30 construction sites (most are tests). Adding a field here is high-churn.
- **`Runtime`** (built from `RunCtx` + the engine's `Inner`): the live execution
  context identifiers/commands read. ~7 construction sites (4 real, 3 test) — the
  **cheaper place** to thread new per-run data. Real sites read from `g` (Inner);
  test sites use `Default::default()` / empty.
- **`Inner`** (behind `Mutex<Inner>` on `ScriptEngine`): shared services and
  registries installed once — `sockets`, `input`, `active` (focused window name),
  `conns` (`ConnReg`), `wins` (`WinReg`). Add a shared, engine-wide thing here and
  expose a `pub fn set_*` on `ScriptEngine`.
- **`StateSnapshot`** (`irc/state.rs`): the per-connection view (nick, channels,
  IAL, isupport, `server_id`, …). If a `$foo` needs a stable per-connection fact,
  add it to `SessionState` + `snapshot()` and read `rt.state.foo` — **zero RunCtx
  churn** (this is how `$cid` avoids threading a new field).
- **`EventVars`** on the Runtime: `$nick`/`$chan`/`$target`/`$1-`/`$snicks`/… Add a
  field here for event-scoped data (defaults empty for non-event runs).

Output is a `Vec<Action>` (`eval.rs`), applied by `apply_actions` in `mod.rs`
(which has the `AppHandle` and all managed state). `apply_actions_depth` carries a
24-deep recursion cap — reuse it for anything that re-dispatches (`/signal`, `/scon`).

Frontend ↔ engine: the store calls thin `#[tauri::command]`s (`script_set_active`,
`script_window_open/close`) to report UI facts the backend can't see. Register each
in `lib.rs generate_handler!` and add a typed wrapper in `src/lib/api.ts`.

---

## 6. Patterns to reuse (don't reinvent)

- **Registry pattern** (for numbered/enumerable things the engine must track):
  a `#[derive(Default)] struct XReg` in `Inner` with `assign`(idempotent)/`forget`/
  `set_active`/`view` methods, a cheap `XView` clone threaded into `Runtime`, and
  `pub fn`s on `ScriptEngine`. See `ConnReg`/`ConnsView` (`$cid`/`$scon`) and
  `WinReg`/`WinView` (`$wid`) — copy one when you need the next.
- **Special-var storage** (engine state read by an identifier, without a new
  Runtime field): stash it in `self.vars` under a NUL-prefixed key that can't
  collide with a real `%var`. See `SOCK_BR_KEY` (`$sockbr`), `V1_KEY`/`V2_KEY`
  (`$v1`/`$v2`).
- **Lazy identifiers** (must NOT pre-expand args): intercept by name in
  `Runtime::expand` *before* the generic arg-expansion and hand the raw args to a
  dedicated handler. `$regsubex` and `$iif` do this — `$iif` expands the condition,
  publishes `$v1`/`$v2`, then expands only the taken branch. Copy this shape for any
  short-circuit/deferred-eval identifier.
- **New `Action` variant**: add to the enum, handle it in `apply_actions_depth`
  (the non-exhaustive match will force you to), respect the depth cap if it
  re-dispatches. `RunOn` (`/scon`/`/scid`) runs a command in another connection's
  context this way.
- **Popup identifiers** (`$style`, `$submenu`, `$snick`): these live in the popup
  *evaluation* path (`eval_popup_labels` in `mod.rs`) and `PopupItem`, not the
  generic identifier dispatch. `$style` uses a Private-Use sentinel char stripped
  at menu-build time.

---

## 7. Gotchas (hard-won — don't relearn these)

- **`UiEvent` serde**: needs BOTH `rename_all = "camelCase"` and
  `rename_all_fields = "camelCase"`, or event *fields* never reach the UI. There's
  a guarding test.
- **Adding a `UiEvent`**: variant (camelCase fields) → handle in `store.ts
  handleEvent` → add to the `IrcEvent` union in `api.ts`. Three places.
- **Detached windows / anything that blocks the main thread**: the
  `open_detached_window` command and any `$input`-reachable command must be `async`
  / run on `spawn_blocking`, or WebView2 deadlocks (blank window). See CLAUDE.md.
- **Disjoint field borrows**: `Runtime { vars: &mut g.vars, active: g.active.clone() }`
  is fine (different fields). But clone a value out of `self.conns` into a local
  before `self.actions.push(...)` if the borrow would otherwise overlap.
- **Sandboxing**: `$file`/`$read`/`/write` go through `sandbox_path` (leaf name
  only, under `scriptdata/`). Keep new file identifiers on that path.
- **Times** are unix seconds for `$file().mtime` etc.; user-facing timestamps use
  `chrono::Local` (the user is in NZ — never show UTC to them).
- **`store.ts` mocks**: `src/state/store.test.ts` mocks `../lib/api`; when a store
  action starts calling a new `api.*`, add it to the mock or the test throws.

---

## 8. Finding the next thing to build

Work `docs/PARITY.md` top-down, but triage honestly:

- **Do now**: pure/self-contained identifiers whose semantics the CHM pins exactly,
  and things a real script (i7.mrc) actually uses.
- **Build the model, then do**: items needing infrastructure jIRC lacks —
  UI-state (`$active`), connection/window ids (`$cid`/`$wid`), etc. Thread it once
  (registry + a reporting command) and a cluster lights up.
- **Skip / defer, and say why**: blocked on absent subsystems (COM/DDE/DLL,
  agents, media, drawing windows, user-access lists), or requiring an exact
  algorithm the CHM doesn't give (`$ansi2mirc` colours, `$hash`).

Tell the user which bucket the remaining work is in; don't silently grind low-value
long-tail items.

---

## 9. Testing patterns

- **Prefer pure unit tests** over end-to-end: `process_message` (`connection.rs`)
  and the engine (`run_alias`/`run_command`/`dispatch_event`, `eval_ident`) are
  pure — test them directly.
- The `ctx()` helper in `mod.rs` tests builds a default `RunCtx`. To test a
  connection/window-scoped identifier, build a `RunCtx` with a custom
  `StateSnapshot { server_id: "s1".into(), ..Default::default() }` (see
  `numeric_connection_ids`, `window_ids`).
- For an identifier, assert via a run: `engine.run_command(&ctx(), "#c",
  "/msg #c x=$foo", &[])` → `vec![Action::Send("PRIVMSG #c :x=...".into())]`.
- Test the **empty/`$null`** and **out-of-range** cases too — that's where fidelity
  bugs hide.

---

## 10. Working style with the user

- The user often says just "carry on" / "keep going" — that means **implement,
  verify, ship, and pick the next item autonomously**, not "ask me what's next".
  Don't stop to request permission for the obvious next step. Do stop for genuine
  scope changes or destructive actions.
- Report **outcomes first**: what shipped, the version, the commit hash, tests
  green. Then the interesting details. Then what's genuinely next.
- Keep the help file (`public/help.html`) honest — it's embedded via `include_str!`
  and shown by `/help`. When you add user-facing identifiers/commands, add a row;
  when you discover it claims something unsupported that now works, fix it.
- When you learn a durable, non-obvious project fact, it belongs in a memory / this
  file / CLAUDE.md — not just the current reply.

---

*Written 2026-07 by Fable 5, after building the mSL popup identifiers, `$file`,
`$v1`/`$v2` (+ lazy `$iif`), `$active`, and the numeric connection/window id
family. The patterns above are the ones those features established — follow them
and the next feature will look like it was written by the same hand.*
