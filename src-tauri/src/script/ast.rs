//! Abstract syntax tree for the mIRC scripting language (mSL) subset.

use serde::Serialize;
use std::collections::HashMap;

/// A single statement within a script body.
#[derive(Debug, Clone)]
pub enum Stmt {
    /// A command invocation: `name` plus the unexpanded argument string.
    Command { name: String, args: String },
    /// `if (cond) { .. } [elseif (cond) { .. }] [else { .. }]`
    If {
        branches: Vec<(String, Vec<Stmt>)>,
        else_body: Option<Vec<Stmt>>,
    },
    /// `while (cond) { .. }`
    While { cond: String, body: Vec<Stmt> },
    /// A `:label` jump target for `/goto`.
    Label(String),
}

/// A user-defined alias: `/name` runs `body`.
#[derive(Debug, Clone)]
pub struct Alias {
    pub name: String,
    pub body: Vec<Stmt>,
    /// `alias -l name`: a local alias — callable from within scripts (other
    /// aliases/events) but not as a user `/command` from the input box.
    pub local: bool,
    /// The `#group` this alias belongs to (if any), for `/enable`/`/disable`.
    pub group: Option<String>,
}

/// An event handler, e.g. `on *:TEXT:*:#:{ .. }`.
#[derive(Debug, Clone)]
pub struct Event {
    /// Event kind, uppercased: TEXT, JOIN, PART, etc.
    pub kind: String,
    /// Matchtext pattern (wildcards), e.g. `*` or `!hello*`. Empty if absent.
    pub pattern: String,
    /// Target pattern, e.g. `#` (any channel), `#chan`, `?` (query). Empty if absent.
    pub target: String,
    pub body: Vec<Stmt>,
    /// The `#group` this handler belongs to (if any), for `/enable`/`/disable`.
    pub group: Option<String>,
}

/// A single item in a popup menu (mIRC `menu` blocks).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PopupItem {
    pub label: String,
    /// The command to run (unexpanded). Empty for separators and submenu parents.
    pub command: String,
    pub separator: bool,
    pub children: Vec<PopupItem>,
}

/// A popup menu definition for one or more contexts (nicklist, channel, …).
#[derive(Debug, Clone)]
pub struct Popup {
    pub contexts: Vec<String>,
    pub items: Vec<PopupItem>,
}

/// One control in a custom dialog.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DialogControl {
    /// text, edit, editbox, button, check, combo, list.
    pub kind: String,
    pub id: String,
    /// Label (text/button/check) or initial value (edit).
    pub label: String,
    /// Initial options for combo/list controls.
    pub options: Vec<String>,
    /// `:default` button (also the Enter key).
    pub default: bool,
    /// `:cancel` button (also Esc; closes the dialog).
    pub cancel: bool,
}

/// A custom dialog definition (`dialog name { … }`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Dialog {
    pub name: String,
    pub title: String,
    pub controls: Vec<DialogControl>,
}

/// A fully compiled script.
#[derive(Debug, Clone, Default)]
pub struct Script {
    pub aliases: Vec<Alias>,
    pub events: Vec<Event>,
    pub popups: Vec<Popup>,
    pub dialogs: Vec<Dialog>,
    /// Declared `#name on/off` script groups: (name, default-enabled).
    pub groups: Vec<(String, bool)>,
}

impl Script {
    pub fn find_alias(&self, name: &str) -> Option<&Alias> {
        self.aliases
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(name))
    }

    /// Like [`find_alias`], but only matches when the alias's `#group` (if any) is
    /// currently enabled — a disabled-group alias isn't callable.
    pub fn find_active_alias(&self, name: &str, vars: &HashMap<String, String>) -> Option<&Alias> {
        self.find_alias(name)
            .filter(|a| self.group_enabled(vars, &a.group))
    }

    /// Whether a def's `#group` is currently enabled. A runtime `/enable`/`/disable`
    /// override (stored in `vars` under a reserved key) wins over the group's
    /// declared `on`/`off` default; ungrouped defs are always enabled.
    pub fn group_enabled(&self, vars: &HashMap<String, String>, group: &Option<String>) -> bool {
        let Some(name) = group else {
            return true;
        };
        if let Some(v) = vars.get(&group_var_key(name)) {
            return v != "0";
        }
        self.groups
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map_or(true, |(_, on)| *on)
    }

    pub fn find_dialog(&self, name: &str) -> Option<&Dialog> {
        self.dialogs.iter().find(|d| d.name.eq_ignore_ascii_case(name))
    }

    pub fn events_of<'a>(&'a self, kind: &str) -> impl Iterator<Item = &'a Event> {
        let kind = kind.to_ascii_uppercase();
        self.events.iter().filter(move |e| e.kind == kind)
    }

    /// Returns the popup items defined for `context` (and `*`-wildcard menus).
    pub fn popup_items(&self, context: &str) -> Vec<PopupItem> {
        let context = context.to_ascii_lowercase();
        let mut items = Vec::new();
        for popup in &self.popups {
            if popup
                .contexts
                .iter()
                .any(|c| c == &context || c == "*")
            {
                items.extend(popup.items.iter().cloned());
            }
        }
        items
    }
}

/// The reserved `vars` key holding a group's runtime enabled-state (set by
/// `/enable`/`/disable`). The NUL bytes keep it from colliding with a user `%var`,
/// which can't contain them.
pub fn group_var_key(name: &str) -> String {
    format!("\u{0}grp\u{0}{}", name.to_ascii_lowercase())
}
