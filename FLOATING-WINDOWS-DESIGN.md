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
scrollback today). So on open, the popped-out window requests a **snapshot** from the main window:

```
popped-out window  ──emit("win:request-snapshot", {bufferKey})──►  main window
main window         ──emit_to(poppedWindow, "win:snapshot", {messages, nicks, …})──►  popped-out
```

(`@window` line content is *also* available straight from the backend `WindowStore` I just built —
a `window_snapshot` command — but routing everything through the main window keeps one code path.)

After the snapshot, both windows are in lock-step via `irc-event`.

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

**Closing the OS window** (its native ✕): treated as dock-back (so the buffer isn't lost) — or as
"close buffer" if you prefer; **open question, see §6**.

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

## 6. Open questions (need your call)

1. **Native ✕ on a popped-out window** → does it **dock back** (keep the buffer) or **close the
   buffer** entirely? (I lean: dock back, so nothing is lost; closing is via the buffer's own close.)
2. **Status/server window** poppable too, or only channels/queries/`@windows`? (You said "all" —
   confirming the server/status window is included.)
3. **Reconnect / restart**: should popped-out windows be **remembered** and re-opened on next launch,
   or always start docked? (Lean: start docked for v1; persistence later.)
4. **One buffer, two places**: disallowed (a buffer is either docked or popped-out, never both) —
   confirming that's the intended behaviour.

---

## 7. Phasing

- **Phase A — pop-out/dock-back skeleton:** the Tauri spawn, single-window mode, snapshot IPC, and
  the two buttons, proven on **one channel**. End state: pop a channel out to an OS window, it's
  live, dock it back with one click.
- **Phase B — all window types + `@window` listbox rendering** (this also finishes the earlier
  Phase 1b): `@windows`, queries, status all poppable; the `WindowList` component.
- **Phase C — polish:** the `⧉` switchbar marker + focus-on-click, native-✕ behaviour, edge cases
  (dock back while a different buffer is active, server disconnect while popped out).

Each phase is independently testable and shippable.
