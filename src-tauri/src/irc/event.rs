//! Typed events emitted from the backend to the frontend.
//!
//! All events are emitted under the Tauri event name [`IRC_EVENT`] with a
//! `type` discriminator so the frontend can route them. Every event carries the
//! `serverId` it belongs to.

use serde::Serialize;

/// The Tauri event channel used for all IRC events.
pub const IRC_EVENT: &str = "irc-event";

/// Direction of a raw protocol line, for the developer/raw console.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    In,
    Out,
}

/// A single member of a channel as seen in a NAMES reply or membership update.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Member {
    pub nick: String,
    /// Mode prefixes in descending rank order, e.g. "@" or "@+".
    pub prefix: String,
}

/// Kind of a textual message.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageKind {
    Privmsg,
    Notice,
}

/// Events sent to the frontend. Serialized as `{ "type": "...", ... }`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum UiEvent {
    /// TCP connection established (before registration completes).
    Connected {
        server_id: String,
    },
    /// Registration completed (RPL_WELCOME received); `nick` is confirmed.
    Registered {
        server_id: String,
        nick: String,
    },
    /// Connection closed.
    Disconnected {
        server_id: String,
        reason: String,
    },
    /// A raw protocol line, for the raw console.
    Raw {
        server_id: String,
        direction: Direction,
        line: String,
    },
    /// A PRIVMSG or NOTICE.
    Message {
        server_id: String,
        kind: MessageKind,
        from: Option<String>,
        target: String,
        text: String,
        /// Server-time (IRCv3 `@time` tag, ISO 8601) when present.
        time: Option<String>,
    },
    Join {
        server_id: String,
        channel: String,
        nick: String,
    },
    Part {
        server_id: String,
        channel: String,
        nick: String,
        reason: Option<String>,
    },
    Quit {
        server_id: String,
        nick: String,
        reason: Option<String>,
        /// Channels (we knew about) the user was in when they quit.
        channels: Vec<String>,
    },
    Kick {
        server_id: String,
        channel: String,
        /// The nick that was kicked.
        nick: String,
        /// The nick that did the kicking.
        by: Option<String>,
        reason: Option<String>,
        /// True when we were the one kicked.
        is_self: bool,
    },
    /// A user's away state changed (via the away-notify capability).
    AwayChange {
        server_id: String,
        nick: String,
        away: bool,
        message: Option<String>,
        /// Channels we share with this user.
        channels: Vec<String>,
    },
    NickChange {
        server_id: String,
        old: String,
        new: String,
    },
    /// Full membership snapshot for a channel (after NAMES completes).
    Names {
        server_id: String,
        channel: String,
        members: Vec<Member>,
    },
    Topic {
        server_id: String,
        channel: String,
        topic: Option<String>,
        set_by: Option<String>,
    },
    /// A mode change on a channel or user.
    Mode {
        server_id: String,
        target: String,
        modes: String,
        /// Who set the mode (nick or server), when known.
        by: Option<String>,
    },
    /// Any server numeric reply not otherwise handled.
    Numeric {
        server_id: String,
        code: u16,
        args: Vec<String>,
    },
    /// Server ISUPPORT (005) info the frontend needs for routing/rendering.
    Isupport {
        server_id: String,
        chan_types: String,
        /// Prefix characters, highest rank first (e.g. "~&@%+" or ".@+").
        prefixes: String,
    },
    /// A formatted WHOIS reply block.
    Whois {
        server_id: String,
        nick: String,
        lines: Vec<String>,
    },
    /// One channel from a LIST/LISTX reply (for the channel-list window).
    ListEntry {
        server_id: String,
        channel: String,
        users: u32,
        topic: String,
    },
    /// End of a LIST/LISTX reply.
    ListEnd {
        server_id: String,
    },
    /// You were invited to a channel.
    Invite {
        server_id: String,
        from: Option<String>,
        channel: String,
    },

    // ---- Script-driven custom dialogs ----
    /// Open a script-defined dialog.
    DialogOpen {
        server_id: String,
        name: String,
        title: String,
        controls: Vec<crate::script::ast::DialogControl>,
    },
    /// Close a dialog.
    DialogClose {
        server_id: String,
        name: String,
    },
    /// Mutate a dialog control (`op` = set/add/clear).
    DialogSet {
        server_id: String,
        dialog: String,
        control: String,
        op: String,
        value: String,
    },
    /// Set/clear a nick-list icon for a nick.
    NickIcon {
        server_id: String,
        nick: String,
        icon: String,
    },
    /// Your own away state changed (RPL_NOWAWAY / RPL_UNAWAY).
    SelfAway {
        server_id: String,
        away: bool,
    },

    // ---- IRCX (Phase 1b) ----
    /// Result of enabling/querying IRCX (numeric 800).
    IrcxState {
        server_id: String,
        version: Option<String>,
        packages: Option<String>,
        max_message_length: Option<String>,
        options: Option<String>,
    },
    /// A single channel/object access entry (numerics 801/804).
    IrcxAccess {
        server_id: String,
        object: String,
        level: Option<String>,
        mask: Option<String>,
        set_by: Option<String>,
        timeout: Option<String>,
        reason: Option<String>,
    },
    /// End of an ACCESS listing (numeric 805).
    IrcxAccessEnd {
        server_id: String,
        object: String,
    },
    /// A single object property value (numeric 818).
    IrcxProp {
        server_id: String,
        object: String,
        name: String,
        value: String,
    },
    /// End of a PROP listing (numeric 819).
    IrcxPropEnd {
        server_id: String,
        object: String,
    },
    /// A whisper (channel-scoped private message visible only to targets).
    Whisper {
        server_id: String,
        from: Option<String>,
        channel: String,
        text: String,
    },
    /// A protocol/connection error string.
    Error {
        server_id: String,
        message: String,
    },
    /// Local text echoed by a script (`/echo`) into a target buffer.
    Echo {
        server_id: String,
        target: String,
        text: String,
    },

    // ---- Script-driven custom windows (@window) ----
    /// Open/create a custom window.
    WindowOpen {
        server_id: String,
        name: String,
        kind: String,
        title: String,
    },
    /// Close a custom window.
    WindowClose {
        server_id: String,
        name: String,
    },
    /// A line operation on a custom window: `op` = add/insert/replace/delete/clear.
    WindowLine {
        server_id: String,
        name: String,
        op: String,
        n: u32,
        text: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_variant_and_fields_as_camelcase() {
        // Variant name lowercased; fields camelCased (the frontend relies on this).
        let ev = UiEvent::Registered {
            server_id: "s1".into(),
            nick: "bob".into(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"type\":\"registered\""), "{json}");
        assert!(json.contains("\"serverId\":\"s1\""), "{json}");

        let topic = UiEvent::Topic {
            server_id: "s1".into(),
            channel: "#c".into(),
            topic: None,
            set_by: Some("op".into()),
        };
        let json = serde_json::to_string(&topic).unwrap();
        assert!(json.contains("\"setBy\":\"op\""), "{json}");
    }
}
