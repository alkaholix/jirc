# jIRC — Detachable Windows (pop-out / dock-back) — Design

**Status:** proposal for review (no code yet).
**Goal:** any jIRC window — server/status, channel, query, or custom `@window` — can be
**popped out** into its own real OS-level window and **docked back** with a single click (no
dragging). Inspired by mIRC's MDI, but going further: detached windows are genuine OS windows.

---

## 1. The model

Every buffer has one of two placements:

| State | What it is |
|---|---|
| **Docked** (default) | A tab/entry in jIRC's switchbar+sidebar, filling the main area when active — exactly today's behaviour. |
| **Popped out** | A separate OS window (a Tauri child window) showing just that buffer. |

Controls:
- On a **docked** window's header: a **`⧉ pop out`** button → detaches it.
- On a **popped-out** window's top-right: a **`⧈ dock back`** button → re-attaches it (one click).
- While popped out, jIRC keeps the buffer's switchbar entry, marked **`⧉`**; clicking it just
  **focuses** the OS window (so you never lose it).

No drag-to-dock, no drag-to-detach — buttons only, as requested. (Dragging the OS window around
is the OS's job; jIRC doesn't manage its position.)

---

## 2. Architecture (Tauri multi-window)

Tauri v2 supports multiple `WebviewWindow`s; each runs the same bundled web app in its own JS
context (its own zustand store). Events emitted with `app.emit(...)` are delivered to **all**
windows — this is the key that makes sync cheap.

```
                 backend (Rust)  ── app.emit("irc-event", …) ─┐ (broadcast to ALL windows)
                       ▲                                       │
        invoke(cmd)    │ commands                              ▼
   ┌───────────────────┴───────────┐        ┌──────────────────────────────┐
   │  MAIN window  (zustand store) │◄──IPC──►│  popped-out window @scores   │
   │  buffers: #mirc, @scores, …   │ snapshot│  renders ONLY @scores        │
   │  renders the docked UI        │  + dock │  (single-window mode)        │
   └───────────────────────────────┘         └──────────────────────────────┘
```

- **Both windows subscribe to `irc-event`.** New messages, `@window` line ops, nick changes, etc.
  reach every window automatically → a popped-out window stays live with zero extra plumbing.
- **The popped-out window renders one buffer** ("single-window mode"), chosen from a route param
  on its URL, e.g. `index.html#/win/<serverId>/<bufferKey>`.

### 2a. The one hard problem: initial contents

`irc-event` only carries *new* activity. A freshly-spawned window needs the buffer's *existing*
contents (scrollback / current `@window` lines) for its first paint.

The buffer's history lives in the **main window's** zustand store (the backend doesn't keep channel
scrollback today). **Implemented (Phase A):** the popping window stashes a snapshot of the buffer
(+ its server) in **shared `localStorage`** under `jirc.detached.<bufferKey>` right before spawning,
and the detached window hydrates from it on mount. Tauri's windows are same-origin, so they share
one `localStorage` partition — this needs **no window-to-window IPC and no extra permissions**, which
is why it was chosen over the `emit`/`emit_to` request/response handshake originally sketched here.

After hydration, both windows stay in lock-step via the app-wide `irc-event` broadcast.

### 2b. Sending (typing) from a popped-out window

Identical to the main window: the input bar calls the same `invoke("scriptRunInput"/"sendMessage"…)`
commands. The backend processes them and broadcasts the result via `irc-event` → both windows update.
Nothing special needed.

---

## 3. Dock / undock flow

**Pop out** (from the main window):
1. Main window calls `invoke("open_detached_window", { bufferKey, title })`.
2. Rust spawns a `WebviewWindow` with url `…#/win/<serverId>/<bufferKey>`.
3. Main window marks that buffer `poppedOut = true` in its store (switchbar shows `⧉`, main area
   no longer renders it).

**Dock back** (the top-right button in the popped-out window):
1. Popped-out window emits `win:dock-back` `{ bufferKey }` (or `invoke("dock_window", …)`).
2. Main window clears `poppedOut` for that buffer (it renders as a normal tab again) and focuses it.
3. The popped-out `WebviewWindow` closes itself.

**Closing the OS window** (its native ✕): **closes the buffer** (part channel / close query / close
`@window`), per §6 — returning a window to jIRC is the separate `⧈ dock back` button, not ✕.

---

## 4. Frontend changes

- **`App.tsx`**: detect single-window mode from the route. If `#/win/<…>` → mount a lean
  `<DetachedWindow bufferKey>` that renders just that buffer (channel: topic+messages+input+nicklist;
  query: messages+input; `@window`: listbox) using the existing components. Else → today's full UI.
- **New `DetachedWindow.tsx`**: hosts one buffer; on mount, request snapshot + subscribe to `irc-event`;
  shows the `⧈ dock back` button top-right.
- **Window header / switchbar** (`SwitchBar`/`Sidebar`): add the `⧉ pop out` control and the
  `⧉ popped-out` marker; clicking a popped-out entry focuses its OS window (`invoke("focus_window")`).
- **`store.ts`**: add `poppedOut` per buffer; handle `win:request-snapshot` (main side) and the
  snapshot/route bootstrapping (detached side). The existing `handleEvent(irc-event)` is reused as-is.
- **`@window` rendering** (Phase 1b of the earlier work): a `WindowList` component renders the
  `WindowStore` lines as a listbox — used both docked and popped-out.

## 5. Backend changes (small)

- `commands.rs`: `open_detached_window`, `dock_window` (or handled purely in JS via events),
  `focus_window`, and optionally `window_snapshot(server_id, name)` for `@window` content.
- `lib.rs`: register them. The `WindowOpen/WindowClose/WindowLine` events already exist.
- No change to the IRC engine; it already broadcasts everything.

---

## 6. Decisions (resolved)

1. The popped-out window's **native ✕ closes the buffer** (part channel / close query / close
   `@window`) — same as closing it inside jIRC. A **separate `⧈ dock back` button** (distinct from
   ✕) is how you return it to jIRC without closing.
2. **All** windows are poppable, **including the server/status window**.
3. Windows **always start docked** on launch; popped-out state is **not persisted** in v1.
4. A buffer is **docked XOR popped-out** — never shown in both places at once.

---

## 7. Phasing

- **Phase A — pop-out/dock-back skeleton — ✅ IMPLEMENTED:** the Tauri spawn (`open_detached_window`
  / `focus_window` / `dock_window`), single-window mode (`#win/<bufferKey>` → `DetachedView`),
  localStorage snapshot hydration, and the `⧉` pop-out / `⧈ dock-back` buttons — proven on any one
  buffer (channel/query/status). Pop a buffer out to its own OS window, it stays live, dock it back
  with one click; a "popped out" placeholder (Focus / Dock-back) holds its spot in the main window.
  *Deferred to Phase C:* native-✕-closes-the-buffer (today native ✕ just closes the OS window and
  leaves the placeholder, which docks it back) and the switchbar `⧉` marker.
- **Phase B — custom `@window` rendering — ✅ IMPLEMENTED:** the backend `@window` engine
  (`/window`, `/aline`/`iline`/`rline`/`dline`, `$window`/`$line`) already emitted
  `WindowOpen`/`WindowClose`/`WindowLine`; the frontend now consumes them. An `@window` is a buffer
  (new kind `"window"`, `▣` icon) and renders through the **existing** `TopicBar + MessageList +
  InputBar` path (window lines are plain rows — no `WindowList` component needed), so it is poppable
  for free via Phase A. Queries/status were already poppable in Phase A. Plain text typed in an
  `@window` is delivered only via `on INPUT` (no PRIVMSG).
- **Phase C — polish — ✅ IMPLEMENTED:** the `⧉` sidebar/switchbar marker on popped-out buffers +
  click-to-focus-its-window; **native ✕ now closes the buffer** (via `close_detached` → the
  `win-close-buffer` broadcast), distinguished from dock-back by a `handledClose` guard; and a
  "Window closed" state in a detached window whose buffer disappears (e.g. server disconnect).

Each phase is independently testable and shippable.
