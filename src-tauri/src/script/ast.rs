//! Abstract syntax tree for the mIRC scripting language (mSL) subset.

use serde::Serialize;
use std::collections::HashMap;

/// A single statement within a script body.
#[derive(Debug, Clone)]
pub enum Stmt {
    /// A command invocation: `name` plus the unexpanded argument string.
    Command {
        name: String,
        args: String,
        line: usize,
    },
    /// `if (cond) { .. } [elseif (cond) { .. }] [else { .. }]`
    If {
        branches: Vec<(String, Vec<Stmt>)>,
        else_body: Option<Vec<Stmt>>,
        line: usize,
    },
    /// `while (cond) { .. }`
    While {
        cond: String,
        body: Vec<Stmt>,
        line: usize,
    },
    /// A `:label` jump target for `/goto`.
    Label { name: String, line: usize },
}

impl Stmt {
    pub fn source_line(&self) -> usize {
        match self {
            Stmt::Command { line, .. }
            | Stmt::If { line, .. }
            | Stmt::While { line, .. }
            | Stmt::Label { line, .. } => *line,
        }
    }
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
    /// The loaded script file that defined this alias. mIRC `alias -l`
    /// visibility is limited to commands executing from this same source.
    pub source: String,
    /// One-based physical line containing the alias definition. This is the
    /// source context exposed by `$scriptline` while the alias runs.
    pub source_line: usize,
}

/// An event handler, e.g. `on *:TEXT:*:#:{ .. }`.
#[derive(Debug, Clone)]
pub struct Event {
    /// The access expression before the kind: `*` (any), a numeric/named level,
    /// exact `+N`, and mIRC event gates such as `me:`, `!`, or `@`.
    pub level: String,
    /// Event kind, uppercased: TEXT, JOIN, PART, etc.
    pub kind: String,
    /// Matchtext pattern (wildcards), e.g. `*` or `!hello*`. Empty if absent.
    pub pattern: String,
    /// Command/numeric selector for a standard top-level `raw` definition.
    /// Empty for named `on` events and CTCP definitions.
    pub selector: String,
    /// Target pattern, e.g. `#` (any channel), `#chan`, `?` (query). Empty if absent.
    pub target: String,
    pub body: Vec<Stmt>,
    /// The `#group` this handler belongs to (if any), for `/enable`/`/disable`.
    pub group: Option<String>,
    /// The loaded script file that defined this handler. Access-level
    /// selection is independent for each remote script in mIRC.
    pub source: String,
    /// One-based physical line containing the event definition.
    pub source_line: usize,
}

/// A single item in a popup menu (mIRC `menu` blocks).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PopupItem {
    pub label: String,
    /// The command to run (unexpanded). Empty for separators and submenu parents.
    pub command: String,
    pub separator: bool,
    /// `$style(1|3)` — show a check mark. Set at menu-build time.
    #[serde(default)]
    pub checked: bool,
    /// `$style(2|3)` — greyed, non-selectable. Set at menu-build time.
    #[serde(default)]
    pub disabled: bool,
    /// The loaded script file that defined this item. This is returned to the
    /// frontend so a deferred click keeps `alias -l` visibility file-local.
    #[serde(default)]
    pub source: String,
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
    /// text, edit, editbox, button, check, radio, box, scroll, combo, list,
    /// link, or tab.
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
    pub ok: bool,
    pub styles: Vec<String>,
    pub enabled: bool,
    pub visible: bool,
    pub tab: String,
}

/// A custom dialog definition (`dialog name { … }`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Dialog {
    pub name: String,
    pub title: String,
    pub controls: Vec<DialogControl>,
    pub width: i32,
    pub height: i32,
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
    /// Loaded remote-script filenames in load order (including empty/UI-only
    /// files, which `$script(N)` must still enumerate).
    pub sources: Vec<String>,
}

impl Script {
    pub fn find_alias(&self, name: &str) -> Option<&Alias> {
        self.aliases
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(name))
    }

    /// Finds an alias callable from the command line. Local aliases are never
    /// visible here, even when a same-named local alias occurs first.
    pub fn find_public_alias(&self, name: &str) -> Option<&Alias> {
        self.aliases
            .iter()
            .find(|a| !a.local && a.name.eq_ignore_ascii_case(name))
    }

    /// Like [`find_alias`], but only matches when the alias's `#group` (if any) is
    /// currently enabled — a disabled-group alias isn't callable.
    pub fn find_active_alias(&self, name: &str, vars: &HashMap<String, String>) -> Option<&Alias> {
        self.find_alias(name)
            .filter(|a| self.group_enabled(vars, &a.group))
    }

    /// Resolves a script-invoked alias using mIRC's file-local visibility. A
    /// definition in the current source wins; otherwise only global aliases
    /// from other sources are eligible.
    pub fn find_active_alias_from(
        &self,
        name: &str,
        vars: &HashMap<String, String>,
        source: &str,
    ) -> Option<&Alias> {
        self.aliases
            .iter()
            .find(|a| {
                a.name.eq_ignore_ascii_case(name)
                    && a.source.eq_ignore_ascii_case(source)
                    && self.group_enabled(vars, &a.group)
            })
            .or_else(|| {
                self.aliases.iter().find(|a| {
                    !a.local
                        && a.name.eq_ignore_ascii_case(name)
                        && self.group_enabled(vars, &a.group)
                })
            })
    }

    /// Assigns a stable source identity to every executable definition parsed
    /// from one script file.
    pub fn set_source(&mut self, source: &str) {
        if !source.is_empty() {
            self.sources.clear();
            self.sources.push(source.to_string());
        }
        for alias in &mut self.aliases {
            alias.source = source.to_string();
        }
        for event in &mut self.events {
            event.source = source.to_string();
        }
        for popup in &mut self.popups {
            set_popup_item_source(&mut popup.items, source);
        }
    }

    /// Appends another independently-parsed script while preserving load order.
    pub fn append(&mut self, mut other: Script) {
        self.aliases.append(&mut other.aliases);
        self.events.append(&mut other.events);
        self.popups.append(&mut other.popups);
        self.dialogs.append(&mut other.dialogs);
        self.groups.append(&mut other.groups);
        self.sources.append(&mut other.sources);
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
        self.dialogs
            .iter()
            .find(|d| d.name.eq_ignore_ascii_case(name))
    }

    pub fn events_of<'a>(&'a self, kind: &str) -> impl Iterator<Item = &'a Event> {
        let kind = kind.to_ascii_uppercase();
        self.events.iter().filter(move |e| e.kind == kind)
    }

    /// Loaded remote-script filenames in load order, with duplicates removed.
    /// A source can contain only aliases or only events, so inspect both lists.
    pub fn source_files(&self) -> Vec<&str> {
        if !self.sources.is_empty() {
            return self.sources.iter().map(String::as_str).collect();
        }
        let mut out = Vec::new();
        for source in self
            .aliases
            .iter()
            .map(|a| a.source.as_str())
            .chain(self.events.iter().map(|e| e.source.as_str()))
        {
            if !source.is_empty()
                && !out
                    .iter()
                    .any(|known: &&str| known.eq_ignore_ascii_case(source))
            {
                out.push(source);
            }
        }
        out
    }

    /// Loaded filenames containing at least one alias, in load order.
    pub fn alias_source_files(&self) -> Vec<&str> {
        let mut out = Vec::new();
        for source in &self.sources {
            if self
                .aliases
                .iter()
                .any(|alias| alias.source.eq_ignore_ascii_case(source))
            {
                out.push(source.as_str());
            }
        }
        if out.is_empty() {
            for alias in &self.aliases {
                if !alias.source.is_empty()
                    && !out
                        .iter()
                        .any(|known: &&str| known.eq_ignore_ascii_case(&alias.source))
                {
                    out.push(alias.source.as_str());
                }
            }
        }
        out
    }

    /// Returns the popup items defined for `context` (and `*`-wildcard menus).
    pub fn popup_items(&self, context: &str) -> Vec<PopupItem> {
        let context = context.to_ascii_lowercase();
        let mut items = Vec::new();
        for popup in &self.popups {
            if popup.contexts.iter().any(|c| c == &context || c == "*") {
                items.extend(popup.items.iter().cloned());
            }
        }
        items
    }
}

fn set_popup_item_source(items: &mut [PopupItem], source: &str) {
    for item in items {
        item.source = source.to_string();
        set_popup_item_source(&mut item.children, source);
    }
}

/// The reserved `vars` key holding a group's runtime enabled-state (set by
/// `/enable`/`/disable`). The NUL bytes keep it from colliding with a user `%var`,
/// which can't contain them.
pub fn group_var_key(name: &str) -> String {
    format!("\u{0}grp\u{0}{}", name.to_ascii_lowercase())
}
