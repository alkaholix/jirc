import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { api, IrcEvent, ServerProfile } from "./lib/api";
import { STATUS, bufferKey, useStore } from "./state/store";
import { Sidebar } from "./components/Sidebar";
import { SwitchBar } from "./components/SwitchBar";
import { TopicBar } from "./components/TopicBar";
import { MessageList } from "./components/MessageList";
import { NickList } from "./components/NickList";
import { InputBar } from "./components/InputBar";
import { ConnectDialog } from "./components/ConnectDialog";
import { SettingsDialog } from "./components/SettingsDialog";
import { ScriptDialog } from "./components/ScriptDialog";
import { ChannelListDialog } from "./components/ChannelListDialog";
import { AutoJoinDialog } from "./components/AutoJoinDialog";
import { TransfersPanel } from "./components/TransfersPanel";
import { dccDetect } from "./state/dcc";
import { ChannelCentral } from "./components/ChannelCentral";
import { ConfirmDialog } from "./components/ConfirmDialog";
import { PromptDialog } from "./components/PromptDialog";
import { UserDialogs } from "./components/UserDialogs";
import { DetachedView } from "./components/DetachedView";
import { thisWindowBufferKey, popOutBuffer, dockBackBuffer, detachedLabel } from "./lib/detach";
import { confirmDialog } from "./state/confirm";
import { promptDialog } from "./state/prompt";
import { routeDialogEvent } from "./state/dialogs";
import { routeNickIconEvent } from "./state/nickIcons";
import { routeAwayEvent } from "./state/away";
import { pollNotify, routeNotifyEvent } from "./state/notify";
import { routeUrlEvent } from "./state/urlGrabber";
import { routeModeEvent } from "./state/channelModes";
import { applyTheme, applyCustomCss, applyChatFont, useSettings } from "./state/settings";

function App() {
  const [dialogOpen, setDialogOpen] = useState(false);
  const [chooserOpen, setChooserOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [scriptOpen, setScriptOpen] = useState(false);
  const [autoJoinOpen, setAutoJoinOpen] = useState(false);
  const [detachedKey] = useState(() => thisWindowBufferKey());
  const theme = useSettings((s) => s.theme);
  const customCss = useSettings((s) => s.customCss);
  const layout = useSettings((s) => s.layout);
  const chatFont = useSettings((s) => s.chatFont);
  const chatFontSize = useSettings((s) => s.chatFontSize);
  const dccIp = useSettings((s) => s.dccIp);
  const dccPortFrom = useSettings((s) => s.dccPortFrom);
  const dccPortTo = useSettings((s) => s.dccPortTo);
  const handleEvent = useStore((s) => s.handleEvent);
  const ensureServer = useStore((s) => s.ensureServer);
  const ensureBuffer = useStore((s) => s.ensureBuffer);
  const setActive = useStore((s) => s.setActive);
  const appendLine = useStore((s) => s.appendLine);
  const active = useStore((s) => (s.active ? s.buffers[s.active] : null));
  const activePoppedOut = useStore((s) => (s.active ? !!s.poppedOut[s.active] : false));
  const hasServers = useStore((s) => Object.keys(s.servers).length > 0);

  useEffect(() => {
    const unlisten = listen<IrcEvent>("irc-event", (e) => {
      handleEvent(e.payload);
      routeDialogEvent(e.payload);
      routeNickIconEvent(e.payload);
      routeAwayEvent(e.payload);
      routeNotifyEvent(e.payload);
      routeUrlEvent(e.payload);
      routeModeEvent(e.payload);
      // Approve/decline an incoming DCC chat — prompt once, in the main window.
      if (e.payload.type === "dccChatOffer" && detachedKey === null) {
        const o = e.payload;
        confirmDialog(
          `${o.nick} wants to start a DCC chat with you (from ${o.ip}). Accept?`,
          { title: "DCC chat request", confirmLabel: "Accept" }
        ).then((ok) => {
          if (ok) api.dccAccept(o.serverId, o.nick, o.ip, o.port).catch(() => {});
        });
      }
      if (e.payload.type === "dccFileOffer" && detachedKey === null) {
        const o = e.payload;
        confirmDialog(
          `${o.nick} wants to send you "${o.filename}" (${o.size} bytes). Download it?`,
          { title: "DCC file offer", confirmLabel: "Download" }
        ).then((ok) => {
          if (ok)
            api.dccRecv(o.serverId, o.nick, o.filename, o.ip, o.port, o.size).catch(() => {});
        });
      }
      if (e.payload.type === "dccLocalHost") dccDetect.set(e.payload.host);
      // A script ran `/server host port [pass]` (e.g. a local bridge): open a
      // server window and connect the native client to it. Main window only.
      if (e.payload.type === "scriptServer" && detachedKey === null) {
        const o = e.payload;
        const serverId = crypto.randomUUID();
        const profile: ServerProfile = {
          id: serverId,
          name: o.host,
          host: o.host,
          port: o.port,
          nick: `Guest${Math.floor(1000 + Math.random() * 9000)}`,
          password: o.pass || undefined,
          tls: false,
          autojoin: [],
        };
        ensureServer(serverId, o.host);
        ensureBuffer(serverId, STATUS, "status");
        setActive(bufferKey(serverId, STATUS));
        appendLine(serverId, STATUS, "status", {
          kind: "system",
          text: `Connecting to ${o.host}:${o.port}…`,
        });
        api.connect(profile).catch((err) =>
          appendLine(serverId, STATUS, "status", { kind: "error", text: `Connect failed: ${err}` })
        );
      }
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, [handleEvent]);

  // A script called $input: show the prompt dialog and send the answer back so
  // the blocked script can resume. Handled only in the main window (avoid dupes).
  useEffect(() => {
    const unlisten = listen<{ id: number; message: string; title: string; default: string }>(
      "script-prompt",
      async (e) => {
        if (detachedKey !== null) return;
        const { id, message, title, default: initial } = e.payload;
        const value = await promptDialog(message, { title, initial });
        invoke("script_prompt_reply", { id, value }).catch(() => {});
      }
    );
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  // A detached window asked to dock back in: clear its popped-out flag and show it.
  useEffect(() => {
    const unlisten = listen<string>("win-dock", (e) => {
      const key = e.payload;
      const st = useStore.getState();
      st.setPoppedOut(key, false);
      if (st.buffers[key]) st.setActive(key);
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  // A detached window was closed via its native ✕: close the buffer and drop the flag.
  useEffect(() => {
    const unlisten = listen<string>("win-close-buffer", (e) => {
      const key = e.payload;
      const st = useStore.getState();
      st.setPoppedOut(key, false);
      st.closeBuffer(key);
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  // Poll the notify/watch list (ISON) every 30s.
  useEffect(() => {
    const id = setInterval(pollNotify, 30000);
    pollNotify();
    return () => clearInterval(id);
  }, []);

  useEffect(() => {
    applyTheme(theme);
  }, [theme]);

  useEffect(() => {
    applyCustomCss(customCss);
  }, [customCss]);

  useEffect(() => {
    applyChatFont(chatFont, chatFontSize);
  }, [chatFont, chatFontSize]);

  // Keep the backend DCC config (advertised IP + listen-port range) in sync.
  useEffect(() => {
    api.dccConfigure(dccIp, dccPortFrom, dccPortTo).catch(() => {});
  }, [dccIp, dccPortFrom, dccPortTo]);

  // Close the "new connection" chooser on Escape.
  useEffect(() => {
    if (!chooserOpen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setChooserOpen(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [chooserOpen]);

  // Detached single-window mode: render just one buffer in its own OS window.
  // (Empty string = a detached window whose route wasn't found; still not the main UI.)
  if (detachedKey !== null) {
    return <DetachedView bufferKey={detachedKey} />;
  }

  const onConnect = async (profile: ServerProfile) => {
    // Every connect opens a NEW server window with a fresh connection id, so
    // connecting to another server never replaces an existing connection.
    // (profile.id identifies the *saved profile*, not the live connection.)
    const serverId = crypto.randomUUID();
    const withId = { ...profile, id: serverId };
    ensureServer(serverId, profile.name);
    ensureBuffer(serverId, STATUS, "status");
    setActive(bufferKey(serverId, STATUS));
    appendLine(serverId, STATUS, "status", {
      kind: "system",
      text: `Connecting to ${profile.host}:${profile.port}${profile.tls ? " (TLS)" : ""} as ${profile.nick}…`,
    });
    try {
      await api.connect(withId);
    } catch (err) {
      appendLine(serverId, STATUS, "status", {
        kind: "error",
        text: `Connect failed: ${err}`,
      });
    }
  };

  // A local console: a status window with no IRC connection, so mSL scripts and
  // socket bots can be run before/without connecting to a server.
  const openLocalConsole = () => {
    const serverId = "local";
    const existed = !!useStore.getState().servers[serverId];
    ensureServer(serverId, "Local");
    ensureBuffer(serverId, STATUS, "status");
    setActive(bufferKey(serverId, STATUS));
    if (!existed) {
      appendLine(serverId, STATUS, "status", {
        kind: "system",
        text: "Local console — no IRC connection. Run scripts and socket bots here.",
      });
    }
  };

  const actions = {
    onAddServer: () => setChooserOpen(true),
    onOpenSettings: () => setSettingsOpen(true),
    onOpenScripts: () => setScriptOpen(true),
    onOpenAutoJoin: () => setAutoJoinOpen(true),
    onOpenHelp: () => api.openHelp().catch(() => {}),
  };

  return (
    <div className={`app layout-${layout}`}>
      {layout === "tree" ? <Sidebar {...actions} /> : <SwitchBar {...actions} />}
      <main className="main">
        {active ? (
          activePoppedOut ? (
            <div className="welcome">
              <h1>Popped out</h1>
              <p>
                {active.kind === "status" ? "This server window" : active.name} is open in its own
                window.
              </p>
              <div className="welcome-actions">
                <button onClick={() => api.focusWindow(detachedLabel(active.key)).catch(() => {})}>
                  Focus its window
                </button>
                <button className="ghost" onClick={() => dockBackBuffer(active.key)}>
                  ⧈ Dock back into jIRC
                </button>
              </div>
            </div>
          ) : (
            <>
              <TopicBar buffer={active} onPopOut={() => popOutBuffer(active.key)} />
              <div className="main-body">
                <div className="chat-pane">
                  <MessageList buffer={active} />
                  <InputBar buffer={active} />
                </div>
                {active.kind === "channel" && <NickList buffer={active} />}
              </div>
            </>
          )
        ) : (
          <div className="welcome">
            <h1>jIRC</h1>
            <p>A modern IRC client with mIRC-style power — standard IRC &amp; IRCX.</p>
            <div className="welcome-actions">
              <button onClick={() => setDialogOpen(true)}>
                {hasServers ? "Add another connection" : "Connect to a server"}
              </button>
              <button className="ghost" onClick={openLocalConsole}>
                Open a local console
              </button>
            </div>
            <p className="welcome-hint">
              A local console is a window with no IRC connection — run mSL scripts and socket bots there.
            </p>
          </div>
        )}
      </main>
      {chooserOpen && (
        <div className="modal-backdrop" onClick={() => setChooserOpen(false)}>
          <div className="modal chooser-modal" onClick={(e) => e.stopPropagation()}>
            <h2>New connection</h2>
            <p className="chooser-lead">
              Connect to an IRC server, or open a local console to run scripts and socket
              bots without connecting.
            </p>
            <div className="welcome-actions">
              <button
                onClick={() => {
                  setChooserOpen(false);
                  setDialogOpen(true);
                }}
              >
                Connect to a server
              </button>
              <button
                className="ghost"
                onClick={() => {
                  setChooserOpen(false);
                  openLocalConsole();
                }}
              >
                Open a local console
              </button>
            </div>
          </div>
        </div>
      )}
      {dialogOpen && <ConnectDialog onClose={() => setDialogOpen(false)} onConnect={onConnect} />}
      {settingsOpen && <SettingsDialog onClose={() => setSettingsOpen(false)} />}
      {scriptOpen && <ScriptDialog onClose={() => setScriptOpen(false)} />}
      {autoJoinOpen && <AutoJoinDialog onClose={() => setAutoJoinOpen(false)} />}
      <ChannelListDialog />
      <ChannelCentral />
      <ConfirmDialog />
      <PromptDialog />
      <UserDialogs />
      <TransfersPanel />
    </div>
  );
}

export default App;
