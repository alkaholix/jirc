//! Custom `@windows` for mSL: `/window`, `/aline`, `/rline`, `/dline`, `/clear`
//! and `$window` / `$line`.
//!
//! The engine holds the authoritative window state here (so `$window`/`$line`
//! read it synchronously), persisted in global state like hash tables. Commands
//! also push `Action`s that `apply_actions` turns into `UiEvent`s so the frontend
//! can mirror/render the window. Line positions are **1-based** (mIRC convention).

use std::collections::{BTreeSet, HashMap};

/// The display kind of a custom window (the default listbox also renders plain
/// text lines).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowKind {
    Listbox,
    Editbox,
    Picture,
}

impl WindowKind {
    pub fn as_str(self) -> &'static str {
        match self {
            WindowKind::Listbox => "listbox",
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
    /// One-based selected line numbers for listbox windows.
    pub selected: BTreeSet<usize>,
    /// Oldest-to-newest click coordinates retained for `$click`.
    pub clicks: Vec<(i32, i32)>,
    pub bitmap_width: u32,
    pub bitmap_height: u32,
    pub bitmap_rgba: Vec<u8>,
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
            selected: BTreeSet::new(),
            clicks: Vec::new(),
            bitmap_width: 0,
            bitmap_height: 0,
            bitmap_rgba: Vec::new(),
        });
    }

    pub fn close(&mut self, name: &str) {
        self.windows.remove(&key(name));
    }

    pub fn record_click(&mut self, name: &str, x: i32, y: i32) {
        if let Some(window) = self.windows.get_mut(&key(name)) {
            window.clicks.push((x, y));
            if window.clicks.len() > 100 {
                window.clicks.remove(0);
            }
        }
    }

    pub fn clear_clicks(&mut self, name: &str) {
        if let Some(window) = self.windows.get_mut(&key(name)) {
            window.clicks.clear();
        }
    }

    pub fn set_bitmap(&mut self, name: &str, width: u32, height: u32, rgba: Vec<u8>) {
        if let Some(window) = self.windows.get_mut(&key(name)) {
            if rgba.len() == width as usize * height as usize * 4 {
                window.bitmap_width = width;
                window.bitmap_height = height;
                window.bitmap_rgba = rgba;
            }
        }
    }

    pub fn dot(&self, name: &str, x: u32, y: u32) -> Option<u32> {
        let window = self.get(name)?;
        if x >= window.bitmap_width || y >= window.bitmap_height {
            return None;
        }
        let offset = (y as usize * window.bitmap_width as usize + x as usize) * 4;
        Some(
            u32::from(window.bitmap_rgba[offset])
                | (u32::from(window.bitmap_rgba[offset + 1]) << 8)
                | (u32::from(window.bitmap_rgba[offset + 2]) << 16),
        )
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
            w.selected = w
                .selected
                .iter()
                .map(|line| if *line >= idx + 1 { line + 1 } else { *line })
                .collect();
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
                w.selected = w
                    .selected
                    .iter()
                    .filter_map(|line| {
                        if *line == i + 1 {
                            None
                        } else if *line > i + 1 {
                            Some(line - 1)
                        } else {
                            Some(*line)
                        }
                    })
                    .collect();
            }
        }
    }

    /// `/clear` — remove all lines.
    pub fn clear(&mut self, name: &str) {
        if let Some(w) = self.windows.get_mut(&key(name)) {
            w.lines.clear();
            w.selected.clear();
        }
    }

    /// `/sline` — select, add, or remove a one-based listbox line.
    pub fn select(&mut self, name: &str, n: usize, add: bool, remove: bool) {
        if let Some(w) = self.windows.get_mut(&key(name)) {
            if !add && !remove {
                w.selected.clear();
            }
            if n == 0 || n > w.lines.len() {
                return;
            }
            if remove {
                w.selected.remove(&n);
            } else {
                w.selected.insert(n);
            }
        }
    }

    pub fn selected_count(&self, name: &str) -> usize {
        self.get(name).map_or(0, |w| w.selected.len())
    }

    pub fn selected_line(&self, name: &str, n: usize) -> Option<usize> {
        self.get(name)
            .and_then(|w| w.selected.iter().nth(n.saturating_sub(1)).copied())
    }

    pub fn is_selected(&self, name: &str, n: usize) -> bool {
        self.get(name).is_some_and(|w| w.selected.contains(&n))
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
        s.select("@list", 2, false, false);
        assert!(s.is_selected("@list", 2));
        assert_eq!(s.selected_line("@list", 1), Some(2));
        assert_eq!(s.line("@list", 2), "TWO");
        s.iline("@list", 1, "zero");
        assert!(s.is_selected("@list", 3));
        assert_eq!(s.line("@list", 1), "zero");
        assert_eq!(s.count("@list"), 4);
        s.dline("@list", 1); // remove "zero"
        assert!(s.is_selected("@list", 2));
        assert_eq!(s.line("@list", 1), "one");
        s.clear("@list");
        assert_eq!(s.count("@list"), 0);
        s.set_bitmap("@list", 1, 1, vec![12, 34, 56, 255]);
        assert_eq!(s.dot("@list", 0, 0), Some(12 | (34 << 8) | (56 << 16)));
        s.close("@list");
        assert!(!s.exists("@list"));
    }
}
