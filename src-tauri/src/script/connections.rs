//! Routine-scoped connection selection and routing for `/scid` and `/scon`.

use std::sync::Arc;

use crate::irc::state::StateSnapshot;

use super::eval::{Action, Runtime};

fn take_argument(raw: &str) -> (&str, &str) {
    let raw = raw.trim_start();
    let mut nesting = 0usize;
    for (index, ch) in raw.char_indices() {
        if ch.is_whitespace() && nesting == 0 {
            return (&raw[..index], raw[index..].trim_start());
        }
        match ch {
            '(' | '[' => nesting += 1,
            ')' | ']' => nesting = nesting.saturating_sub(1),
            _ => {}
        }
    }
    (raw, "")
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ConnectionStatus {
    Connected,
    Connecting,
    Disconnected,
}

impl ConnectionStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::Connecting => "connecting",
            Self::Disconnected => "disconnected",
        }
    }
    pub(super) fn from_state(state: &StateSnapshot) -> Self {
        if state.connect_time != 0 {
            Self::Connected
        } else if !state.server_id.is_empty() {
            Self::Connecting
        } else {
            Self::Disconnected
        }
    }

    fn matches(self, mask: u32) -> bool {
        mask == 0
            || match self {
                Self::Connected => mask & 1 != 0,
                Self::Connecting => mask & (2 | 4) != 0,
                Self::Disconnected => mask & (2 | 8) != 0,
            }
    }
}

#[derive(Clone)]
pub(crate) struct ConnectionContext {
    pub my_nick: String,
    pub network: String,
    pub server: String,
    pub state: Arc<StateSnapshot>,
    pub status: ConnectionStatus,
    pub target: String,
    pub routed: bool,
}

#[derive(Default)]
pub(crate) struct ConnectionScope {
    original: Option<ConnectionContext>,
    routed: bool,
    actions_start: usize,
    status: Option<ConnectionStatus>,
}

pub(crate) struct ConnectionFrame {
    current: ConnectionContext,
    original: Option<ConnectionContext>,
}

impl Runtime<'_> {
    pub(crate) fn connection_status(&self) -> ConnectionStatus {
        self.connection_scope
            .status
            .unwrap_or_else(|| ConnectionStatus::from_state(&self.state))
    }
    fn connection_context(&self) -> ConnectionContext {
        ConnectionContext {
            my_nick: self.my_nick.clone(),
            network: self.network.clone(),
            server: self.server.clone(),
            state: self.state.clone(),
            status: self.connection_status(),
            target: self.event.target.clone(),
            routed: self.connection_scope.routed,
        }
    }

    pub(crate) fn flush_connection_actions(&mut self) {
        let start = self.connection_scope.actions_start;
        if self.connection_scope.routed
            && self.state.server_id != self.conns.source_server_id
            && start < self.actions.len()
        {
            let actions = self.actions.split_off(start);
            self.actions.push(Action::WithConnection {
                server_id: self.state.server_id.clone(),
                my_nick: self.my_nick.clone(),
                network: self.network.clone(),
                server: self.server.clone(),
                actions,
            });
        }
        self.connection_scope.actions_start = self.actions.len();
    }

    fn restore_connection(&mut self, context: ConnectionContext) {
        self.flush_connection_actions();
        self.my_nick = context.my_nick;
        self.network = context.network;
        self.server = context.server;
        self.state = context.state;
        self.event.target = context.target;
        self.connection_scope.routed = context.routed;
        self.connection_scope.status = Some(context.status);
    }

    pub(crate) fn enter_connection_routine(&mut self) -> ConnectionFrame {
        self.flush_connection_actions();
        ConnectionFrame {
            current: self.connection_context(),
            original: self.connection_scope.original.take(),
        }
    }

    pub(crate) fn leave_connection_routine(&mut self, frame: ConnectionFrame) {
        self.restore_connection(frame.current);
        self.connection_scope.original = frame.original;
    }

    fn select_connection(&mut self, server_id: &str) -> Option<&'static str> {
        if self.state.server_id == server_id {
            return Some("active");
        }
        if self
            .connection_scope
            .original
            .as_ref()
            .is_some_and(|original| original.state.server_id == server_id)
        {
            let original = self.connection_scope.original.take().unwrap();
            self.restore_connection(original);
            return Some("reset");
        }
        let Some(context) = self.conns.contexts.get(server_id).cloned() else {
            return None;
        };
        if self.connection_scope.original.is_none() {
            self.connection_scope.original = Some(self.connection_context());
        }
        self.restore_connection(context);
        Some("set")
    }

    fn run_connection_command(&mut self, context: ConnectionContext, command: &str) {
        let frame = self.enter_connection_routine();
        self.restore_connection(context);
        let body = super::parser::parse_body(command);
        self.run(&body);
        self.leave_connection_routine(frame);
    }

    pub(crate) fn cmd_connection(&mut self, name: &str, raw: &str, show: bool) {
        // Keep jIRC's single-evaluation boundary: expand only selection syntax
        // here. The untouched subcommand runs once in its destination context;
        // data returned by $1- or another identifier never becomes source.
        let (first_raw, tail) = take_argument(raw);
        let first = self.expand(first_raw);
        let flagged = first.trim().starts_with('-');
        let rest = if flagged { tail } else { raw.trim_start() };
        let mut reset = false;
        let mut all = false;
        let mut display = false;
        let mut mask = 0;
        if flagged {
            let flags = first.trim();
            reset = flags.contains('r');
            all = flags.contains('a');
            display = flags.contains('s') && show;
            if let Some((_, suffix)) = flags.split_once('t') {
                let digits: String = suffix.chars().take_while(char::is_ascii_digit).collect();
                mask = digits.parse::<u32>().unwrap_or(0);
            }
        }

        if all {
            if rest.is_empty() {
                return;
            }
            let targets: Vec<_> = self
                .conns
                .entries
                .iter()
                .filter_map(|(_, id)| self.conns.contexts.get(id))
                .filter(|context| context.status.matches(mask))
                .cloned()
                .collect();
            for context in targets {
                if self.halted {
                    break;
                }
                self.run_connection_command(context, rest);
            }
            return;
        }

        if reset {
            if rest.is_empty() {
                let feedback = if let Some(original) = self.connection_scope.original.take() {
                    self.restore_connection(original);
                    "reset"
                } else {
                    "active"
                };
                if display {
                    self.display_connection(feedback);
                }
            } else {
                let context = self
                    .connection_scope
                    .original
                    .clone()
                    .unwrap_or_else(|| self.connection_context());
                self.run_connection_command(context, rest);
            }
            return;
        }

        if rest.is_empty() {
            if display {
                self.display_connection("active");
            }
            return;
        }
        let (selector, command) = if flagged {
            let (selector, command) = take_argument(rest);
            (self.expand(selector), command)
        } else {
            (first, tail)
        };
        let Ok(number) = selector.trim().parse::<i64>() else {
            return;
        };
        let target = if number <= 0 {
            self.conns.entries.first()
        } else if name == "scon" {
            usize::try_from(number)
                .ok()
                .and_then(|n| n.checked_sub(1))
                .and_then(|index| self.conns.entries.get(index))
        } else {
            u32::try_from(number)
                .ok()
                .and_then(|wanted| self.conns.entries.iter().find(|(cid, _)| *cid == wanted))
        }
        .map(|(_, id)| id.clone());
        let Some(target) = target else {
            if number > 0 {
                let kind = if name == "scon" { "number" } else { "ID" };
                self.actions.push(Action::Echo {
                    target: "(status)".into(),
                    text: format!("* No such connection {kind}: {number}"),
                });
            }
            return;
        };
        if command.trim().is_empty() {
            if let Some(feedback) = self.select_connection(&target) {
                if display {
                    self.display_connection(feedback);
                }
            }
        } else if let Some(context) = self
            .conns
            .contexts
            .get(&target)
            .cloned()
            .filter(|context| context.status.matches(mask))
        {
            self.run_connection_command(context, command.trim_start());
        }
    }

    fn display_connection(&mut self, kind: &str) {
        let cid = self.conns.cid_of(&self.state.server_id);
        if cid == 0 {
            return;
        }
        let prefix = match kind {
            "set" => "* Set connection ID to:",
            "reset" => "* Reset connection ID to:",
            _ => "* Active connection ID:",
        };
        self.actions.push(Action::Echo {
            target: "(status)".into(),
            text: format!("{prefix} {cid}"),
        });
    }
}

#[cfg(test)]
#[path = "connection_tests.rs"]
mod tests;
