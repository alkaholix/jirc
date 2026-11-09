//! Script-visible dialog state. Commands update this before emitting UI actions.

use std::collections::{BTreeSet, HashMap};

use super::ast::{Dialog, DialogControl};
use super::eval::Runtime;

fn key(value: &str) -> String {
    value.to_ascii_lowercase()
}

#[derive(Clone)]
struct Control {
    definition: DialogControl,
    value: String,
    options: Vec<String>,
    selected: BTreeSet<usize>,
    edited: bool,
}

impl Control {
    fn new(definition: DialogControl) -> Self {
        let options = definition.options.clone();
        let value = match definition.kind.as_str() {
            "check" | "radio" => "0".into(),
            "list" | "combo" => options.first().cloned().unwrap_or_default(),
            "scroll" => definition
                .styles
                .iter()
                .position(|style| style == "pos")
                .and_then(|index| definition.styles.get(index + 1))
                .cloned()
                .unwrap_or_else(|| "0".into()),
            _ => definition.label.clone(),
        };
        let selected =
            if matches!(definition.kind.as_str(), "list" | "combo") && !options.is_empty() {
                BTreeSet::from([1])
            } else {
                BTreeSet::new()
            };
        Self {
            definition,
            value,
            options,
            selected,
            edited: false,
        }
    }

    fn is_list(&self) -> bool {
        matches!(self.definition.kind.as_str(), "list" | "combo")
    }

    fn multiple(&self) -> bool {
        self.definition.kind == "list"
            && self
                .definition
                .styles
                .iter()
                .any(|style| matches!(style.as_str(), "multsel" | "extsel"))
    }

    fn selection_text(&mut self) {
        self.value = self
            .selected
            .iter()
            .filter_map(|index| self.options.get(index - 1))
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
    }

    fn lines(&self) -> Vec<String> {
        if self.is_list() {
            self.options.clone()
        } else {
            self.text().split('\n').map(str::to_string).collect()
        }
    }

    fn text(&self) -> &str {
        if matches!(self.definition.kind.as_str(), "check" | "radio") {
            &self.definition.label
        } else {
            &self.value
        }
    }
}

struct OpenDialog {
    controls: Vec<Control>,
    focus: String,
    active_tab: String,
}

#[derive(Default)]
pub(crate) struct DialogStore {
    dialogs: HashMap<String, OpenDialog>,
}

impl DialogStore {
    pub(crate) fn open(&mut self, name: &str, definition: &Dialog) {
        self.dialogs.insert(
            key(name),
            OpenDialog {
                controls: definition
                    .controls
                    .iter()
                    .cloned()
                    .map(Control::new)
                    .collect(),
                focus: String::new(),
                active_tab: definition
                    .controls
                    .iter()
                    .find(|control| control.kind == "tab")
                    .map(|control| control.id.clone())
                    .unwrap_or_default(),
            },
        );
    }

    pub(crate) fn contains(&self, name: &str) -> bool {
        self.dialogs.contains_key(&key(name))
    }

    pub(crate) fn close(&mut self, name: &str) {
        self.dialogs.remove(&key(name));
    }

    pub(crate) fn rename(&mut self, old: &str, new: &str) {
        if !new.is_empty() {
            if let Some(dialog) = self.dialogs.remove(&key(old)) {
                self.dialogs.insert(key(new), dialog);
            }
        }
    }

    fn control(&self, name: &str, id: &str) -> Option<&Control> {
        self.dialogs
            .get(&key(name))?
            .controls
            .iter()
            .find(|control| control.definition.id.eq_ignore_ascii_case(id))
    }

    pub(crate) fn has_control(&self, name: &str, id: &str) -> bool {
        self.control(name, id).is_some()
    }

    pub(crate) fn line_count(&self, name: &str, id: &str) -> usize {
        self.control(name, id)
            .map_or(0, |control| control.lines().len())
    }

    /// Merge user input, not stale copies of script-owned options or styles.
    pub(crate) fn update_event(&mut self, event: &super::eval::EventVars) {
        // Creation snapshots can arrive after the opening alias has already
        // changed a control. They contain defaults, not new user input.
        if event.dialog_event.eq_ignore_ascii_case("init") {
            return;
        }
        let control = event.dialog_control.to_ascii_lowercase();
        let values = if control.is_empty() || control == "0" {
            event.did.clone()
        } else {
            event
                .did
                .iter()
                .filter(|(key, _)| {
                    let key = key.to_ascii_lowercase();
                    key == control || key.ends_with(&format!("\0{control}"))
                })
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        };
        self.update_snapshot(&event.dialog_name, &values);
    }

    pub(crate) fn update_snapshot(&mut self, name: &str, values: &HashMap<String, String>) {
        let Some(dialog) = self.dialogs.get_mut(&key(name)) else {
            return;
        };
        for control in &mut dialog.controls {
            let id = control.definition.id.clone();
            let incoming = values
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(&id))
                .map(|(_, value)| value);
            if control.is_list() {
                if let Some(indices) = values.get(&format!("\u{0}selected\u{0}{id}")) {
                    control.selected = indices
                        .split(',')
                        .filter_map(|value| value.parse::<usize>().ok())
                        .filter(|index| *index > 0 && *index <= control.options.len())
                        .collect();
                    if !control.multiple() {
                        control.selected = control
                            .selected
                            .iter()
                            .next()
                            .copied()
                            .into_iter()
                            .collect();
                    }
                    control.selection_text();
                } else if let Some(value) = incoming {
                    control.selected = value
                        .split('\n')
                        .filter_map(|value| {
                            control
                                .options
                                .iter()
                                .position(|option| option == value)
                                .map(|index| index + 1)
                        })
                        .collect();
                    if !control.multiple() {
                        control.selected = control
                            .selected
                            .iter()
                            .next()
                            .copied()
                            .into_iter()
                            .collect();
                    }
                    control.value = if control.definition.kind == "combo"
                        && control
                            .definition
                            .styles
                            .iter()
                            .any(|style| style == "edit")
                    {
                        value.clone()
                    } else {
                        control
                            .selected
                            .iter()
                            .filter_map(|index| control.options.get(index - 1))
                            .cloned()
                            .collect::<Vec<_>>()
                            .join("\n")
                    };
                }
            } else if matches!(
                control.definition.kind.as_str(),
                "edit" | "editbox" | "check" | "radio" | "scroll"
            ) {
                if let Some(value) = incoming {
                    control.value = value.clone();
                }
            }
            if let Some(edited) = values.get(&format!("\u{0}edited\u{0}{id}")) {
                control.edited = edited == "true";
            }
        }
    }

    pub(crate) fn apply(&mut self, name: &str, id: &str, op: &str, value: &str) {
        let Some(dialog) = self.dialogs.get_mut(&key(name)) else {
            return;
        };
        let Some(index) = dialog
            .controls
            .iter()
            .position(|control| control.definition.id.eq_ignore_ascii_case(id))
        else {
            return;
        };
        let control = &mut dialog.controls[index];
        match op {
            "enable" => control.definition.enabled = true,
            "disable" => control.definition.enabled = false,
            "show" => control.definition.visible = true,
            "hide" => control.definition.visible = false,
            "focus" => dialog.focus = control.definition.id.clone(),
            "tab" => {
                dialog.focus = control.definition.id.clone();
                dialog.active_tab = control.definition.id.clone();
            }
            "edited-reset" => control.edited = false,
            "default" => {
                for (candidate, control) in dialog.controls.iter_mut().enumerate() {
                    control.definition.default = candidate == index;
                }
            }
            "clear" => {
                if control.is_list() {
                    control.options.clear();
                    control.selected.clear();
                }
                if !matches!(control.definition.kind.as_str(), "check" | "radio") {
                    control.value.clear();
                }
                control.definition.label.clear();
                control.edited = false;
            }
            "add" if control.is_list() => control.options.push(value.to_string()),
            "add" => {
                control.definition.label.push_str(value);
                if !matches!(control.definition.kind.as_str(), "check" | "radio") {
                    control.value.push_str(value);
                }
            }
            "insert" | "replace" | "delete" if control.is_list() => {
                let (position, text) = value.split_once(' ').unwrap_or((value, ""));
                let position = position.parse::<usize>().unwrap_or(1).max(1) - 1;
                if op == "insert" {
                    let position = position.min(control.options.len());
                    control.options.insert(position, text.to_string());
                    control.selected = control
                        .selected
                        .iter()
                        .map(|index| if *index > position { index + 1 } else { *index })
                        .collect();
                } else if position < control.options.len() {
                    if op == "replace" {
                        control.options[position] = text.to_string();
                    } else {
                        control.options.remove(position);
                        control.selected = control
                            .selected
                            .iter()
                            .filter_map(|index| {
                                if *index == position + 1 {
                                    None
                                } else {
                                    Some(if *index > position + 1 {
                                        index - 1
                                    } else {
                                        *index
                                    })
                                }
                            })
                            .collect();
                    }
                }
                control.selection_text();
            }
            "check" | "check-keep" | "uncheck" | "uncheck-keep" | "indeterminate"
                if control.is_list() =>
            {
                let number = value
                    .split_whitespace()
                    .next()
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                if op.starts_with("uncheck") {
                    if number == 0 && op == "uncheck" {
                        control.selected.clear();
                    } else {
                        control.selected.remove(&number);
                    }
                } else if op != "indeterminate" {
                    if op != "check-keep" || !control.multiple() {
                        control.selected.clear();
                    }
                    if number > 0 && number <= control.options.len() {
                        control.selected.insert(number);
                    }
                }
                control.selection_text();
            }
            "check" | "check-keep" | "uncheck" | "uncheck-keep" | "indeterminate"
                if matches!(control.definition.kind.as_str(), "check" | "radio") =>
            {
                control.value = if op.starts_with("uncheck") {
                    "0"
                } else if op == "indeterminate" {
                    "2"
                } else {
                    "1"
                }
                .into();
            }
            // Character selection uses additional native edit-line semantics;
            // consume it without replacing edit text with a checkbox value.
            "check" | "check-keep" | "uncheck" | "uncheck-keep" | "indeterminate" => {}
            "icon" | "set" => {
                control.value = value.to_string();
                control.definition.label = value.to_string();
            }
            "range" => {
                control.definition.styles.retain(|style| style != "range");
                control.definition.styles.push("range".into());
                control
                    .definition
                    .styles
                    .extend(value.split_whitespace().map(str::to_string));
            }
            _ => {}
        }
    }
}

fn requested_control(rt: &Runtime<'_>, args: &[String]) -> (String, String, Option<usize>) {
    let first = args.first().map(String::as_str).unwrap_or("");
    let current = &rt.event.dialog_name;
    let store = rt.dialogs.lock().unwrap();
    let named = args.len() >= 3
        || current.is_empty()
        || store.contains(first)
        || first.eq_ignore_ascii_case(current)
        || (args.len() >= 2
            && !store.has_control(current, first)
            && !rt.event.did.contains_key(first)
            && first.parse::<u32>().is_err());
    if named {
        (
            first.to_string(),
            args.get(1).cloned().unwrap_or_default(),
            args.get(2).and_then(|value| value.parse().ok()),
        )
    } else {
        (
            current.clone(),
            first.to_string(),
            args.get(1).and_then(|value| value.parse().ok()),
        )
    }
}

fn snapshot_control(rt: &Runtime<'_>, dialog: &str, id: &str) -> Option<Control> {
    if let Some(control) = rt.dialogs.lock().unwrap().control(dialog, id).cloned() {
        return Some(control);
    }
    // Preserve injected/legacy event snapshots without leaking A's values into
    // an explicit request for B, and without inventing globally open dialogs.
    if !dialog.eq_ignore_ascii_case(&rt.event.dialog_name) {
        return None;
    }
    let value = rt.event.did.get(id)?.clone();
    let options: Vec<String> = rt
        .event
        .did
        .get(&format!("\u{0}options\u{0}{id}"))
        .filter(|value| !value.is_empty())
        .map(|value| value.split('\n').map(str::to_string).collect())
        .unwrap_or_default();
    let selected = value
        .split('\n')
        .filter_map(|value| {
            options
                .iter()
                .position(|option| option == value)
                .map(|index| index + 1)
        })
        .collect();
    Some(Control {
        definition: DialogControl {
            kind: if !options.is_empty() {
                "list"
            } else if matches!(value.as_str(), "0" | "1" | "2") {
                "check"
            } else {
                "edit"
            }
            .into(),
            id: id.into(),
            label: value.clone(),
            options: options.clone(),
            default: false,
            cancel: false,
            ok: false,
            styles: Vec::new(),
            enabled: rt
                .event
                .did
                .get(&format!("\u{0}enabled\u{0}{id}"))
                .is_some_and(|value| value == "true"),
            visible: rt
                .event
                .did
                .get(&format!("\u{0}visible\u{0}{id}"))
                .is_some_and(|value| value == "true"),
            tab: String::new(),
        },
        value,
        options,
        selected,
        edited: rt
            .event
            .did
            .get(&format!("\u{0}edited\u{0}{id}"))
            .is_some_and(|value| value == "true"),
    })
}

pub(crate) fn did_value(rt: &Runtime<'_>, args: &[String], prop: &str) -> String {
    if args.is_empty() {
        return rt.event.dialog_control.clone();
    }
    let (dialog, id, line) = requested_control(rt, args);
    let Some(control) = snapshot_control(rt, &dialog, &id) else {
        return match prop {
            "isid" | "visible" | "enabled" | "edited" => "$false".into(),
            "lines" | "sel" | "state" | "len" => "0".into(),
            _ => String::new(),
        };
    };
    let text = if let Some(line) = line.filter(|line| *line > 0) {
        control.lines().get(line - 1).cloned().unwrap_or_default()
    } else {
        control.text().to_string()
    };
    match prop {
        "len" => text.chars().count().to_string(),
        "lines" => control.lines().len().to_string(),
        "state" => {
            if matches!(control.definition.kind.as_str(), "check" | "radio") {
                control.value
            } else {
                "0".into()
            }
        }
        "sel" => {
            if line == Some(0) {
                control.selected.len().to_string()
            } else {
                control
                    .selected
                    .iter()
                    .nth(line.unwrap_or(1).saturating_sub(1))
                    .copied()
                    .unwrap_or(0)
                    .to_string()
            }
        }
        "seltext" => {
            if control.is_list() {
                control
                    .selected
                    .iter()
                    .next()
                    .and_then(|index| control.options.get(index - 1))
                    .cloned()
                    .unwrap_or_default()
            } else {
                String::new()
            }
        }
        "visible" => if control.definition.visible {
            "$true"
        } else {
            "$false"
        }
        .into(),
        "enabled" => if control.definition.enabled {
            "$true"
        } else {
            "$false"
        }
        .into(),
        "isid" => "$true".into(),
        "edited" => if control.edited { "$true" } else { "$false" }.into(),
        "next" | "prev" => {
            let store = rt.dialogs.lock().unwrap();
            store
                .dialogs
                .get(&key(&dialog))
                .and_then(|dialog| {
                    let index = dialog
                        .controls
                        .iter()
                        .position(|control| control.definition.id.eq_ignore_ascii_case(&id))?;
                    let index = if prop == "next" {
                        index.checked_add(1)
                    } else {
                        index.checked_sub(1)
                    }?;
                    dialog
                        .controls
                        .get(index)
                        .map(|control| control.definition.id.clone())
                })
                .unwrap_or_else(|| {
                    rt.event
                        .did
                        .get(&format!("\u{0}{prop}\u{0}{id}"))
                        .cloned()
                        .unwrap_or_default()
                })
        }
        _ => text,
    }
}

pub(crate) fn control_lines(rt: &Runtime<'_>, dialog: &str, id: &str) -> Vec<String> {
    snapshot_control(rt, dialog, id)
        .map(|control| control.lines())
        .unwrap_or_default()
}

impl Runtime<'_> {
    pub(crate) fn queue_dialog_mutation(&mut self, dialog: &str, id: &str, op: &str, value: &str) {
        self.dialogs.lock().unwrap().apply(dialog, id, op, value);
        self.actions.push(super::eval::Action::DialogSet {
            dialog: dialog.to_string(),
            control: id.to_string(),
            op: op.to_string(),
            value: value.to_string(),
        });
    }
}
