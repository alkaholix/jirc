import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
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
import { ChannelCentral } from "./components/ChannelCentral";
import { ConfirmDialog } from "./components/ConfirmDialog";
import { PromptDialog } from "./components/PromptDialog";
import { UserDialogs } from "./components/UserDialogs";
import { DetachedView } from "./components/DetachedView";
import { parseDetachedRoute, popOutBuffer, dockBackBuffer, detachedLabel } from "./lib/detach";
import { routeDialogEvent } from "./state/dialogs";
import { routeNickIconEvent } from "./state/nickIcons";
import { routeAwayEvent } from "./state/away";
import { pollNotify, routeNotifyEvent } from "./state/notify";
import { routeUrlEvent } from "./state/urlGrabber";
import { routeModeEvent } from "./state/channelModes";
import { applyTheme, applyCustomCss, applyChatFont, useSettings } from "./state/settings";

function App() {
  const [dialogOpen, setDialogOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [scriptOpen, setScriptOpen] = useState(false);
  const [detachedKey] = useState(() => parseDetachedRoute(window.location.hash));
  const theme = useSettings((s) => s.theme);
  const customCss = useSettings((s) => s.customCss);
  const layout = useSettings((s) => s.layout);
  const chatFont = useSettings((s) => s.chatFont);
  const chatFontSize = useSettings((s) => s.chatFontSize);
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
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, [handleEvent]);

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

  // Detached single-window mode: render just one buffer in its own OS window.
  if (detachedKey) {
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
    onAddServer: () => setDialogOpen(true),
    onOpenSettings: () => setSettingsOpen(true),
    onOpenScripts: () => setScriptOpen(true),
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
      {dialogOpen && <ConnectDialog onClose={() => setDialogOpen(false)} onConnect={onConnect} />}
      {settingsOpen && <SettingsDialog onClose={() => setSettingsOpen(false)} />}
      {scriptOpen && <ScriptDialog onClose={() => setScriptOpen(false)} />}
      <ChannelListDialog />
      <ChannelCentral />
      <ConfirmDialog />
      <PromptDialog />
      <UserDialogs />
    </div>
  );
}

export default App;
