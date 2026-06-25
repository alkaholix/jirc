//! A recursive-descent parser for the mSL subset into [`Script`].
//!
//! Supports `alias name { body }`, `on level:EVENT:...:{ body }`, and within a
//! body: `/commands`, `if (cond) { } elseif (cond) { } else { }`, and
//! `while (cond) { }`. Statements are separated by newlines or `|`; lines
//! beginning with `;` are comments.

use super::ast::{Alias, Dialog, DialogControl, Event, Popup, PopupItem, Script, Stmt};

const MATCHTEXT_EVENTS: &[&str] =
    &["TEXT", "ACTION", "NOTICE", "WALLOPS", "CTCP", "CTCPREPLY", "RAW"];

struct Cursor {
    chars: Vec<char>,
    pos: usize,
}

impl Cursor {
    fn new(s: &str) -> Self {
        Cursor {
            chars: s.chars().collect(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn eof(&self) -> bool {
        self.pos >= self.chars.len()
    }

    /// Skips spaces/tabs/newlines, statement separators (`|`), and `;` comments.
    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() || c == '|' => {
                    self.pos += 1;
                }
                Some(';') => {
                    while let Some(c) = self.peek() {
                        self.pos += 1;
                        if c == '\n' {
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
    }

    fn skip_inline_ws(&mut self) {
        while matches!(self.peek(), Some(c) if c == ' ' || c == '\t') {
            self.pos += 1;
        }
    }

    /// Reads an identifier word (letters/digits/_).
    fn read_word(&mut self) -> String {
        let mut w = String::new();
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c == '_' {
                w.push(c);
                self.pos += 1;
            } else {
                break;
            }
        }
        w
    }

    /// With the cursor on '(', returns the balanced inner text and consumes it.
    fn read_parens(&mut self) -> String {
        let mut depth = 0;
        let mut out = String::new();
        while let Some(c) = self.bump() {
            match c {
                '(' => {
                    depth += 1;
                    if depth > 1 {
                        out.push(c);
                    }
                }
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push(c);
                }
                _ => out.push(c),
            }
        }
        out
    }

    /// With the cursor on '{', returns the balanced inner text and consumes it.
    fn read_block(&mut self) -> String {
        let mut depth = 0;
        let mut out = String::new();
        while let Some(c) = self.bump() {
            match c {
                '{' => {
                    depth += 1;
                    if depth > 1 {
                        out.push(c);
                    }
                }
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push(c);
                }
                _ => out.push(c),
            }
        }
        out
    }

    /// Reads until end-of-line or top-level `|` (a single statement's text).
    fn read_statement_text(&mut self) -> String {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if c == '\n' || c == '|' {
                break;
            }
            out.push(c);
            self.pos += 1;
        }
        out.trim().to_string()
    }

    /// Looks ahead (without consuming) at the next word after trivia.
    fn peek_word(&mut self) -> String {
        let save = self.pos;
        self.skip_trivia();
        let w = self.read_word();
        self.pos = save;
        w
    }
}

/// Parses full script text.
pub fn parse(src: &str) -> Script {
    let mut script = Script::default();
    let mut cur = Cursor::new(src);
    // The `#group` the following top-level defs belong to (a range set by
    // `#name on|off` and cleared by `#name end`).
    let mut current_group: Option<String> = None;
    loop {
        cur.skip_trivia();
        if cur.eof() {
            break;
        }
        // `#name on|off|end` — a group marker (a range, not a braced block).
        if cur.peek() == Some('#') {
            cur.pos += 1;
            let name = cur.read_word();
            cur.skip_inline_ws();
            let state = cur.read_nonspace().to_ascii_lowercase();
            if state == "end" {
                current_group = None;
            } else if !name.is_empty() {
                let on = state != "off";
                if !script.groups.iter().any(|(n, _)| *n == name) {
                    script.groups.push((name.clone(), on));
                }
                current_group = Some(name);
            }
            continue;
        }
        let word = cur.read_word();
        match word.to_ascii_lowercase().as_str() {
            "alias" => {
                cur.skip_inline_ws();
                // Optional switches before the name, e.g. `-l` for a local alias.
                let mut local = false;
                while cur.peek() == Some('-') {
                    let flag = cur.read_nonspace();
                    if flag.eq_ignore_ascii_case("-l") {
                        local = true;
                    }
                    cur.skip_inline_ws();
                }
                let name = cur.read_nonspace();
                cur.skip_inline_ws();
                if cur.peek() == Some('{') {
                    let body = cur.read_block();
                    script.aliases.push(Alias {
                        name,
                        body: parse_stmts(&body),
                        local,
                        group: current_group.clone(),
                    });
                }
            }
            "on" => {
                // Read the header up to the opening brace or end of line.
                let start = cur.pos;
                let mut header = String::new();
                let mut hit_brace = false;
                while let Some(c) = cur.peek() {
                    if c == '{' {
                        hit_brace = true;
                        break;
                    }
                    if c == '\n' {
                        break;
                    }
                    header.push(c);
                    cur.pos += 1;
                }
                if hit_brace {
                    let body = cur.read_block();
                    if let Some(mut ev) = parse_event_header(&header, parse_stmts(&body)) {
                        ev.group = current_group.clone();
                        script.events.push(ev);
                    }
                } else if let Some(mut ev) = parse_braceless_event(&header) {
                    // A one-liner: `on *:TEXT:!cmd:#:/msg $chan hi`.
                    ev.group = current_group.clone();
                    script.events.push(ev);
                } else {
                    // No command tail and no brace on this line: the body's `{`
                    // is on a following line — rescan past the newline for it.
                    cur.pos = start;
                    let mut header = String::new();
                    while let Some(c) = cur.peek() {
                        if c == '{' {
                            break;
                        }
                        header.push(c);
                        cur.pos += 1;
                    }
                    if cur.peek() == Some('{') {
                        let body = cur.read_block();
                        if let Some(mut ev) = parse_event_header(&header, parse_stmts(&body)) {
                            ev.group = current_group.clone();
                            script.events.push(ev);
                        }
                    }
                }
            }
            "menu" => {
                // menu <context[,context...]> { items }
                let mut header = String::new();
                while let Some(c) = cur.peek() {
                    if c == '{' {
                        break;
                    }
                    header.push(c);
                    cur.pos += 1;
                }
                if cur.peek() == Some('{') {
                    let body = cur.read_block();
                    let contexts: Vec<String> = header
                        .trim()
                        .split(',')
                        .map(|c| c.trim().to_ascii_lowercase())
                        .filter(|c| !c.is_empty())
                        .collect();
                    if !contexts.is_empty() {
                        script.popups.push(Popup {
                            contexts,
                            items: parse_popup_body(&body),
                        });
                    }
                }
            }
            "dialog" => {
                // dialog <name> { controls }
                cur.skip_inline_ws();
                let name = cur.read_nonspace();
                let mut header = String::new();
                while let Some(c) = cur.peek() {
                    if c == '{' {
                        break;
                    }
                    header.push(c);
                    cur.pos += 1;
                }
                if cur.peek() == Some('{') {
                    let body = cur.read_block();
                    if !name.is_empty() {
                        script.dialogs.push(parse_dialog(name, &body));
                    }
                }
            }
            _ => {
                // Unknown top-level token: skip to end of line.
                while let Some(c) = cur.bump() {
                    if c == '\n' {
                        break;
                    }
                }
            }
        }
    }
    script
}

impl Cursor {
    /// Reads until whitespace or `{`.
    fn read_nonspace(&mut self) -> String {
        let mut w = String::new();
        while let Some(c) = self.peek() {
            if c.is_whitespace() || c == '{' {
                break;
            }
            w.push(c);
            self.pos += 1;
        }
        w
    }
}

fn parse_event_header(header: &str, body: Vec<Stmt>) -> Option<Event> {
    // header like " *:TEXT:*:#:"
    let fields: Vec<&str> = header.trim().split(':').map(|f| f.trim()).collect();
    if fields.len() < 2 {
        return None;
    }
    let kind = fields[1].to_ascii_uppercase();
    let (pattern, target) = if MATCHTEXT_EVENTS.contains(&kind.as_str()) {
        (
            fields.get(2).copied().unwrap_or("").to_string(),
            fields.get(3).copied().unwrap_or("").to_string(),
        )
    } else {
        (
            String::new(),
            fields.get(2).copied().unwrap_or("").to_string(),
        )
    };
    Some(Event {
        kind,
        pattern,
        target,
        body,
        group: None,
    })
}

/// Parses a one-liner `on` event whose body is the trailing command on the same
/// line (no braces), e.g. `*:TEXT:!ping:#:/msg $chan pong`. Returns None when
/// there is no command tail, so the caller can fall back to a `{` on a later
/// line. `splitn` is used so colons inside the command (timestamps, URLs) are
/// preserved.
fn parse_braceless_event(header: &str) -> Option<Event> {
    let mut top = header.trim().splitn(2, ':');
    let _level = top.next()?;
    let after_level = top.next()?;
    let mut ev = after_level.splitn(2, ':');
    let kind = ev.next()?.trim().to_ascii_uppercase();
    if kind.is_empty() {
        return None;
    }
    let rest = ev.next().unwrap_or("");
    let (pattern, target, command) = if kind == "RAW" {
        // on *:RAW:<numeric>:<command> — matchtext, no target field.
        let mut p = rest.splitn(2, ':');
        let matchtext = p.next().unwrap_or("").trim().to_string();
        let command = p.next().unwrap_or("").trim().to_string();
        (matchtext, String::new(), command)
    } else if MATCHTEXT_EVENTS.contains(&kind.as_str()) {
        let mut p = rest.splitn(3, ':');
        let matchtext = p.next().unwrap_or("").trim().to_string();
        let target = p.next().unwrap_or("").trim().to_string();
        let command = p.next().unwrap_or("").trim().to_string();
        (matchtext, target, command)
    } else {
        let mut p = rest.splitn(2, ':');
        let target = p.next().unwrap_or("").trim().to_string();
        let command = p.next().unwrap_or("").trim().to_string();
        (String::new(), target, command)
    };
    if command.is_empty() {
        return None;
    }
    Some(Event {
        kind,
        pattern,
        target,
        body: parse_body(&command),
        group: None,
    })
}

/// Parses a snippet of statements (e.g. a single command line for a timer).
pub fn parse_body(src: &str) -> Vec<Stmt> {
    parse_stmts(src)
}

/// Splits a line into words, keeping "double-quoted" segments (which may contain
/// spaces) as single tokens. Used by dialog control definitions.
fn tokenize_quoted(line: &str) -> Vec<String> {
    let mut toks = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut have = false;
    for c in line.chars() {
        match c {
            '"' => {
                in_q = !in_q;
                have = true;
            }
            c if c.is_whitespace() && !in_q => {
                if have {
                    toks.push(std::mem::take(&mut cur));
                    have = false;
                }
            }
            c => {
                cur.push(c);
                have = true;
            }
        }
    }
    if have {
        toks.push(cur);
    }
    toks
}

/// Parses a `dialog` body: a `title "…"` line plus one control per line, each
/// `<kind> <id> ["label" | "option"…] [:default] [:cancel]`.
fn parse_dialog(name: String, src: &str) -> Dialog {
    let mut dialog = Dialog {
        name,
        title: String::new(),
        controls: Vec::new(),
    };
    for raw in src.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        let toks = tokenize_quoted(line);
        let Some(kind) = toks.first().map(|s| s.to_ascii_lowercase()) else {
            continue;
        };
        if kind == "title" {
            dialog.title = toks.get(1..).map(|r| r.join(" ")).unwrap_or_default();
            continue;
        }
        if !matches!(kind.as_str(), "text" | "edit" | "editbox" | "button" | "check" | "combo" | "list") {
            continue;
        }
        let Some(id) = toks.get(1).cloned() else { continue };
        let rest = &toks[2.min(toks.len())..];
        let default = rest.iter().any(|t| t.eq_ignore_ascii_case(":default"));
        let cancel = rest.iter().any(|t| t.eq_ignore_ascii_case(":cancel"));
        let plain: Vec<String> = rest.iter().filter(|t| !t.starts_with(':')).cloned().collect();
        let (label, options) = if kind == "combo" || kind == "list" {
            (String::new(), plain)
        } else {
            (plain.first().cloned().unwrap_or_default(), Vec::new())
        };
        dialog.controls.push(DialogControl {
            kind,
            id,
            label,
            options,
            default,
            cancel,
        });
    }
    dialog
}

/// Parses a popup-menu body. Each line is `[dots]Label:command`, where the
/// number of leading dots is the nesting depth, `-` is a separator, and a line
/// with no `:command` is a submenu parent.
fn parse_popup_body(src: &str) -> Vec<PopupItem> {
    // Flatten lines into (depth, item) then assemble into a tree.
    let mut flat: Vec<(usize, PopupItem)> = Vec::new();
    for raw in src.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        let depth = line.chars().take_while(|c| *c == '.').count();
        let rest = line[depth..].trim();
        if rest == "-" {
            flat.push((
                depth,
                PopupItem {
                    label: String::new(),
                    command: String::new(),
                    separator: true,
                    children: Vec::new(),
                },
            ));
            continue;
        }
        let (label, command) = match rest.split_once(':') {
            Some((l, c)) => (l.trim().to_string(), c.trim().to_string()),
            None => (rest.to_string(), String::new()),
        };
        flat.push((
            depth,
            PopupItem {
                label,
                command,
                separator: false,
                children: Vec::new(),
            },
        ));
    }
    let mut idx = 0;
    assemble_popup(&flat, &mut idx, 0)
}

/// Recursively assembles flat (depth, item) entries into a tree.
fn assemble_popup(flat: &[(usize, PopupItem)], idx: &mut usize, depth: usize) -> Vec<PopupItem> {
    let mut out = Vec::new();
    while *idx < flat.len() {
        let (d, item) = &flat[*idx];
        if *d < depth {
            break;
        }
        if *d == depth {
            let mut node = item.clone();
            *idx += 1;
            node.children = assemble_popup(flat, idx, depth + 1);
            out.push(node);
        } else {
            // Deeper than expected without a parent — skip defensively.
            *idx += 1;
        }
    }
    out
}

/// Parses a body into a list of statements.
fn parse_stmts(src: &str) -> Vec<Stmt> {
    let mut cur = Cursor::new(src);
    parse_stmts_cursor(&mut cur)
}

fn parse_stmts_cursor(cur: &mut Cursor) -> Vec<Stmt> {
    let mut stmts = Vec::new();
    loop {
        cur.skip_trivia();
        if cur.eof() {
            break;
        }
        let kw = cur.peek_word().to_ascii_lowercase();
        match kw.as_str() {
            "if" => {
                if let Some(s) = parse_if(cur) {
                    stmts.push(s);
                }
            }
            "while" => {
                if let Some(s) = parse_while(cur) {
                    stmts.push(s);
                }
            }
            "elseif" | "else" => break, // handled by the enclosing if
            _ => {
                let text = cur.read_statement_text();
                if let Some(stmt) = parse_command(&text) {
                    stmts.push(stmt);
                }
            }
        }
    }
    stmts
}

fn parse_command(text: &str) -> Option<Stmt> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    // A line beginning with `:` is a goto label (`:start`).
    if let Some(label) = text.strip_prefix(':') {
        let label = label.split_whitespace().next().unwrap_or("").to_string();
        if label.is_empty() {
            return None;
        }
        return Some(Stmt::Label(label));
    }
    let rest = text.strip_prefix('/').unwrap_or(text);
    let (name, args) = match rest.split_once(char::is_whitespace) {
        Some((n, a)) => (n.to_string(), a.trim().to_string()),
        None => (rest.to_string(), String::new()),
    };
    Some(Stmt::Command { name, args })
}

fn parse_if(cur: &mut Cursor) -> Option<Stmt> {
    cur.skip_trivia();
    let _ = cur.read_word(); // "if"
    let mut branches = Vec::new();
    let cond = read_cond_and_block(cur, &mut branches)?;
    let _ = cond;

    let mut else_body = None;
    loop {
        let next = cur.peek_word().to_ascii_lowercase();
        if next == "elseif" {
            cur.skip_trivia();
            let _ = cur.read_word();
            read_cond_and_block(cur, &mut branches)?;
        } else if next == "else" {
            cur.skip_trivia();
            let _ = cur.read_word();
            else_body = Some(read_branch_body(cur));
            break;
        } else {
            break;
        }
    }

    Some(Stmt::If {
        branches,
        else_body,
    })
}

/// Reads `(cond)` then a body and appends to `branches`. Returns Some(()) on ok.
fn read_cond_and_block(cur: &mut Cursor, branches: &mut Vec<(String, Vec<Stmt>)>) -> Option<()> {
    cur.skip_inline_ws();
    cur.skip_trivia();
    if cur.peek() != Some('(') {
        return None;
    }
    // First bracket group (inner, unwrapped — keeps single-group conditions as
    // before). Then extend across trailing `&&`/`||` operands so mixed-paren
    // conditions parse: `if ($2==X) && $y==z { ... }`.
    let mut cond = cur.read_parens();
    loop {
        let save = cur.pos;
        cur.skip_inline_ws();
        let op = match (cur.peek(), cur.chars.get(cur.pos + 1).copied()) {
            (Some('&'), Some('&')) => "&&",
            (Some('|'), Some('|')) => "||",
            _ => {
                cur.pos = save;
                break;
            }
        };
        cur.pos += 2;
        cur.skip_inline_ws();
        let operand = if cur.peek() == Some('(') {
            format!("({})", cur.read_parens())
        } else {
            read_cond_operand(cur)
        };
        cond.push_str(&format!(" {op} {}", operand.trim()));
    }
    let body = read_branch_body(cur);
    branches.push((cond.trim().to_string(), body));
    Some(())
}

/// Reads a parenless condition operand (after `&&`/`||`) up to the next logical
/// operator, body brace, `|`, or end of line.
fn read_cond_operand(cur: &mut Cursor) -> String {
    let mut out = String::new();
    while let Some(c) = cur.peek() {
        if c == '{' || c == '\n' || c == '|' {
            break;
        }
        if c == '&' && cur.chars.get(cur.pos + 1) == Some(&'&') {
            break;
        }
        out.push(c);
        cur.pos += 1;
    }
    out
}

/// Reads the body of an `if`/`elseif`/`else`/`while`: either a `{ }` block (on
/// the same or the next line, Allman-style), or — mIRC's common shorthand — a
/// single brace-less statement that runs to end-of-line or the next `|`.
fn read_branch_body(cur: &mut Cursor) -> Vec<Stmt> {
    cur.skip_inline_ws();
    if cur.peek() == Some('{') {
        return parse_stmts(&cur.read_block());
    }
    // Allman style: the opening brace is on a following line.
    let save = cur.pos;
    cur.skip_trivia();
    if cur.peek() == Some('{') {
        return parse_stmts(&cur.read_block());
    }
    cur.pos = save;
    // Brace-less: a single statement up to end-of-line / `|`.
    parse_stmts(&cur.read_statement_text())
}

fn parse_while(cur: &mut Cursor) -> Option<Stmt> {
    cur.skip_trivia();
    let _ = cur.read_word(); // "while"
    cur.skip_inline_ws();
    cur.skip_trivia();
    if cur.peek() != Some('(') {
        return None;
    }
    let cond = cur.read_parens();
    let body = read_branch_body(cur);
    Some(Stmt::While {
        cond: cond.trim().to_string(),
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_alias() {
        let s = parse("alias hello { /msg $chan hi there }");
        assert_eq!(s.aliases.len(), 1);
        assert_eq!(s.aliases[0].name, "hello");
        assert_eq!(s.aliases[0].body.len(), 1);
        assert!(!s.aliases[0].local);
    }

    #[test]
    fn parses_local_alias_flag() {
        // `alias -l name` must register `name` (not `-l`) as a local alias.
        let s = parse("alias -l helper { /echo hi }");
        assert_eq!(s.aliases.len(), 1);
        assert_eq!(s.aliases[0].name, "helper");
        assert!(s.aliases[0].local);
        assert_eq!(s.aliases[0].body.len(), 1);
        // an underscore/digit name (e.g. i7f_init) parses intact
        let s = parse("alias -l i7f_init { /echo hi }");
        assert_eq!(s.find_alias("i7f_init").map(|a| a.local), Some(true));
    }

    #[test]
    fn parses_text_event() {
        let s = parse("on *:TEXT:!ping*:#:{ /msg $chan pong }");
        assert_eq!(s.events.len(), 1);
        assert_eq!(s.events[0].kind, "TEXT");
        assert_eq!(s.events[0].pattern, "!ping*");
        assert_eq!(s.events[0].target, "#");
    }

    #[test]
    fn parses_join_event_without_matchtext() {
        let s = parse("on *:JOIN:#:{ /msg $chan welcome $nick }");
        assert_eq!(s.events[0].kind, "JOIN");
        assert_eq!(s.events[0].target, "#");
        assert_eq!(s.events[0].pattern, "");
    }

    #[test]
    fn parses_if_elseif_else() {
        let s = parse("alias t { if (%x == 1) { /echo one } elseif (%x == 2) { /echo two } else { /echo other } }");
        match &s.aliases[0].body[0] {
            Stmt::If { branches, else_body } => {
                assert_eq!(branches.len(), 2);
                assert!(else_body.is_some());
            }
            _ => panic!("expected if"),
        }
    }

    #[test]
    fn parses_while() {
        let s = parse("alias t { while (%i < 3) { /inc %i } }");
        assert!(matches!(s.aliases[0].body[0], Stmt::While { .. }));
    }

    #[test]
    fn parses_braceless_if_elseif_else() {
        // The single-statement (no-brace) form, with elseif/else on later lines.
        let s = parse(
            "alias t {\n  if (%x == 1) /echo one\n  elseif (%x == 2) /echo two\n  else /echo other\n}",
        );
        match &s.aliases[0].body[0] {
            Stmt::If { branches, else_body } => {
                assert_eq!(branches.len(), 2);
                assert_eq!(branches[0].1.len(), 1); // one statement in the body
                assert!(else_body.as_ref().is_some_and(|b| b.len() == 1));
            }
            _ => panic!("expected if"),
        }
    }

    #[test]
    fn parses_braceless_while_and_trailing_sibling() {
        let s = parse("alias t { while (%i < 3) /inc %i }");
        assert!(matches!(s.aliases[0].body[0], Stmt::While { .. }));
        // A brace-less `if` body stops at `|`; the rest is a sibling statement.
        let s = parse("alias t { if (%x) /echo a | /echo b }");
        assert_eq!(s.aliases[0].body.len(), 2);
        assert!(matches!(s.aliases[0].body[0], Stmt::If { .. }));
        assert!(matches!(&s.aliases[0].body[1], Stmt::Command { name, .. } if name == "echo"));
    }

    #[test]
    fn parses_popup_menu_with_submenu() {
        let s = parse(
            "menu nicklist,channel {\n  Whois:/whois $1\n  Ops\n  .Op:/mode $chan +o $1\n  .Deop:/mode $chan -o $1\n  -\n  Slap:/me slaps $1\n}",
        );
        assert_eq!(s.popups.len(), 1);
        let p = &s.popups[0];
        assert_eq!(p.contexts, vec!["nicklist", "channel"]);
        // top-level: Whois, Ops(submenu), separator, Slap
        assert_eq!(p.items.len(), 4);
        assert_eq!(p.items[0].label, "Whois");
        assert_eq!(p.items[0].command, "/whois $1");
        assert_eq!(p.items[1].label, "Ops");
        assert_eq!(p.items[1].children.len(), 2);
        assert_eq!(p.items[1].children[0].command, "/mode $chan +o $1");
        assert!(p.items[2].separator);
        let items = s.popup_items("nicklist");
        assert_eq!(items.len(), 4);
        assert!(s.popup_items("query").is_empty());
    }

    #[test]
    fn parses_dialog() {
        let s = parse(
            "dialog greeter {\n  title \"Say hi\"\n  text info \"Who?\"\n  edit name\n  combo dest \"#a\" \"#b\"\n  button send \"Send\" :default\n  button cancel \"Cancel\" :cancel\n}",
        );
        assert_eq!(s.dialogs.len(), 1);
        let d = &s.dialogs[0];
        assert_eq!(d.name, "greeter");
        assert_eq!(d.title, "Say hi");
        assert_eq!(d.controls.len(), 5);
        assert_eq!(d.controls[0].kind, "text");
        assert_eq!(d.controls[0].label, "Who?");
        assert_eq!(d.controls[2].kind, "combo");
        assert_eq!(d.controls[2].options, vec!["#a", "#b"]);
        assert!(d.controls[3].default);
        assert!(d.controls[4].cancel);
    }

    #[test]
    fn multiple_statements_split_on_pipe_and_newline() {
        let s = parse("alias t {\n /echo a | /echo b\n /echo c\n }");
        assert_eq!(s.aliases[0].body.len(), 3);
    }
}
