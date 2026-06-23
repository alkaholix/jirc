//! Custom `@windows` for mSL: `/window`, `/aline`, `/rline`, `/dline`, `/clear`
//! and `$window` / `$line`.
//!
//! The engine holds the authoritative window state here (so `$window`/`$line`
//! read it synchronously), persisted in global state like hash tables. Commands
//! also push `Action`s that `apply_actions` turns into `UiEvent`s so the frontend
//! can mirror/render the window. Line positions are **1-based** (mIRC convention).

use std::collections::HashMap;

/// The display kind of a custom window (Phase 1 renders listbox/text the same).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowKind {
    Listbox,
    Text,
    Editbox,
    Picture,
}

impl WindowKind {
    pub fn as_str(self) -> &'static str {
        match self {
            WindowKind::Listbox => "listbox",
            WindowKind::Text => "text",
            WindowKind::Editbox => "editbox",
            WindowKind::Picture => "picture",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Window {
    pub kind: WindowKind,
    pub title: String,
    pub lines: Vec<String>,
}

#[derive(Default)]
pub struct WindowStore {
    /// Keyed by the lowercased window name (including the leading `@`).
    windows: HashMap<String, Window>,
}

fn key(name: &str) -> String {
    name.trim().to_lowercase()
}

impl WindowStore {
    pub fn open(&mut self, name: &str, kind: WindowKind, title: &str) {
        self.windows.entry(key(name)).or_insert_with(|| Window {
            kind,
            title: title.to_string(),
            lines: Vec::new(),
        });
    }

    pub fn close(&mut self, name: &str) {
        self.windows.remove(&key(name));
    }

    pub fn exists(&self, name: &str) -> bool {
        self.windows.contains_key(&key(name))
    }

    pub fn get(&self, name: &str) -> Option<&Window> {
        self.windows.get(&key(name))
    }

    /// `/aline` — append a line.
    pub fn aline(&mut self, name: &str, text: &str) {
        if let Some(w) = self.windows.get_mut(&key(name)) {
            w.lines.push(text.to_string());
        }
    }

    /// `/iline` — insert a line at 1-based position N (append if past the end).
    pub fn iline(&mut self, name: &str, n: usize, text: &str) {
        if let Some(w) = self.windows.get_mut(&key(name)) {
            let idx = n.saturating_sub(1).min(w.lines.len());
            w.lines.insert(idx, text.to_string());
        }
    }

    /// `/rline` — replace line N (1-based).
    pub fn rline(&mut self, name: &str, n: usize, text: &str) {
        if let Some(w) = self.windows.get_mut(&key(name)) {
            if let Some(slot) = n.checked_sub(1).and_then(|i| w.lines.get_mut(i)) {
                *slot = text.to_string();
            }
        }
    }

    /// `/dline` — delete line N (1-based).
    pub fn dline(&mut self, name: &str, n: usize) {
        if let Some(w) = self.windows.get_mut(&key(name)) {
            if let Some(i) = n.checked_sub(1).filter(|&i| i < w.lines.len()) {
                w.lines.remove(i);
            }
        }
    }

    /// `/clear` — remove all lines.
    pub fn clear(&mut self, name: &str) {
        if let Some(w) = self.windows.get_mut(&key(name)) {
            w.lines.clear();
        }
    }

    /// `$line(@w,N)` — the Nth line (1-based).
    pub fn line(&self, name: &str, n: usize) -> String {
        self.get(name)
            .and_then(|w| n.checked_sub(1).and_then(|i| w.lines.get(i)))
            .cloned()
            .unwrap_or_default()
    }

    /// `$window(@w).lines` — line count.
    pub fn count(&self, name: &str) -> usize {
        self.get(name).map_or(0, |w| w.lines.len())
    }

    /// Open window names, sorted (for `$window(N)`).
    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.windows.keys().cloned().collect();
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_line_ops() {
        let mut s = WindowStore::default();
        s.open("@list", WindowKind::Listbox, "My List");
        assert!(s.exists("@LIST")); // case-insensitive
        s.aline("@list", "one");
        s.aline("@list", "two");
        s.aline("@list", "three");
        assert_eq!(s.count("@list"), 3);
        assert_eq!(s.line("@list", 2), "two");
        s.rline("@list", 2, "TWO");
        assert_eq!(s.line("@list", 2), "TWO");
        s.iline("@list", 1, "zero");
        assert_eq!(s.line("@list", 1), "zero");
        assert_eq!(s.count("@list"), 4);
        s.dline("@list", 1); // remove "zero"
        assert_eq!(s.line("@list", 1), "one");
        s.clear("@list");
        assert_eq!(s.count("@list"), 0);
        s.close("@list");
        assert!(!s.exists("@list"));
    }
}
