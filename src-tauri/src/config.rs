//! Configuration types shared between the frontend and backend.

use serde::{Deserialize, Serialize};

/// A server connection profile. Sent from the frontend when opening a
/// connection; persistence of profiles to disk arrives in Phase 2.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProfile {
    /// Stable identifier for this connection. Generated if omitted.
    #[serde(default)]
    pub id: Option<String>,
    /// Human-readable network name, e.g. "Libera.Chat".
    pub name: String,
    pub host: String,
    pub port: u16,
    /// Connect over TLS.
    #[serde(default)]
    pub tls: bool,
    /// Skip TLS certificate verification (for self-signed test servers).
    #[serde(default)]
    pub tls_insecure: bool,
    /// Enable IRCX mode after registration (sends the `IRCX` command).
    #[serde(default)]
    pub ircx: bool,
    /// Attempt SASL PLAIN authentication during CAP negotiation.
    #[serde(default)]
    pub sasl: bool,
    /// SASL/NickServ account name (defaults to `nick`).
    #[serde(default)]
    pub account: Option<String>,
    /// Account password, used for SASL and/or NickServ (secret).
    #[serde(default)]
    pub account_password: Option<String>,
    /// Identify to NickServ after registration (when not using SASL).
    #[serde(default)]
    pub nickserv: bool,
    /// Automatically reconnect on unexpected disconnects (default true).
    #[serde(default = "default_true")]
    pub auto_reconnect: bool,
    /// Optional SOCKS5 proxy.
    #[serde(default)]
    pub proxy: Option<Proxy>,
    pub nick: String,
    /// Alternative nickname to try if `nick` is in use.
    #[serde(default)]
    pub alt_nick: Option<String>,
    /// Defaults to `nick` when absent.
    #[serde(default)]
    pub username: Option<String>,
    /// Defaults to `nick` when absent.
    #[serde(default)]
    pub realname: Option<String>,
    /// Optional server password (PASS).
    #[serde(default)]
    pub password: Option<String>,
    /// Channels to join automatically after registration.
    #[serde(default)]
    pub autojoin: Vec<String>,
}

fn default_true() -> bool {
    true
}

/// A SOCKS5 proxy configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Proxy {
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
}

impl ServerProfile {
    pub fn username(&self) -> &str {
        self.username.as_deref().unwrap_or(&self.nick)
    }

    pub fn realname(&self) -> &str {
        self.realname.as_deref().unwrap_or(&self.nick)
    }

    pub fn account(&self) -> &str {
        self.account.as_deref().unwrap_or(&self.nick)
    }
}
