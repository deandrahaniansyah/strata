//! SQL editor: syntax highlighting, line numbers, current-statement tint, autocomplete,
//! auto-closing brackets, keyword upper-casing and comment toggling.

use super::p;
use egui::text::{CCursor, CCursorRange, LayoutJob};
use egui::{Color32, FontId, Key, Modifiers, TextFormat, Ui};
use std::collections::HashMap;

pub const KEYWORDS: &[&str] = &[
    "ADD", "ALL", "ALTER", "ANALYZE", "AND", "ANY", "AS", "ASC", "AUTO_INCREMENT", "BEGIN", "BETWEEN", "BIGINT", "BOOLEAN", "BY", "CASCADE", "CASE",
    "CAST", "CHAR", "CHECK", "COLLATE", "COLUMN", "COMMENT", "COMMIT", "CONCURRENTLY", "CONFLICT", "CONSTRAINT", "CREATE", "CROSS", "CURRENT_DATE",
    "CURRENT_TIMESTAMP", "DATABASE", "DATE", "DECIMAL", "DEFAULT", "DELETE", "DESC", "DESCRIBE", "DISTINCT", "DO", "DROP", "ELSE", "END", "ENGINE",
    "EXCEPT", "EXISTS", "EXPLAIN", "EXTENSION", "FALSE", "FETCH", "FILTER", "FIRST", "FLOAT", "FOR", "FOREIGN", "FROM", "FULL", "FUNCTION", "GRANT",
    "GROUP", "HAVING", "IF", "ILIKE", "IN", "INDEX", "INNER", "INSERT", "INT", "INTEGER", "INTERSECT", "INTERVAL", "INTO", "IS", "JOIN", "JSON",
    "JSONB", "KEY", "LAST", "LATERAL", "LEFT", "LIKE", "LIMIT", "MATERIALIZED", "MERGE", "NATURAL", "NEXT", "NOT", "NOTHING", "NULL", "NULLS",
    "NUMERIC", "OFFSET", "ON", "ONLY", "OR", "ORDER", "OUTER", "OVER", "PARTITION", "PRAGMA", "PRIMARY", "PROCEDURE", "REAL", "RECURSIVE",
    "REFERENCES", "REFRESH", "RENAME", "REPLACE", "RESTRICT", "RETURNING", "RETURNS", "REVOKE", "RIGHT", "ROLLBACK", "ROW", "ROWS", "SCHEMA",
    "SELECT", "SEQUENCE", "SERIAL", "SET", "SHOW", "SMALLINT", "TABLE", "TEMPORARY", "TEXT", "THEN", "TIME", "TIMESTAMP", "TIMESTAMPTZ", "TO",
    "TOP", "TRANSACTION", "TRIGGER", "TRUE", "TRUNCATE", "UNION", "UNIQUE", "UPDATE", "USE", "USING", "UUID", "VACUUM", "VALUES", "VARCHAR",
    "VIEW", "WHEN", "WHERE", "WINDOW", "WITH",
];

pub const FUNCTIONS: &[&str] = &[
    "ABS", "AVG", "COALESCE", "CONCAT", "COUNT", "DATE_TRUNC", "EXTRACT", "GREATEST", "IFNULL", "JSON_AGG", "JSON_EXTRACT", "LEAST", "LENGTH",
    "LOWER", "MAX", "MIN", "NOW", "NULLIF", "ROUND", "ROW_NUMBER", "STRING_AGG", "SUBSTRING", "SUM", "TO_CHAR", "TRIM", "UPPER",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tok {
    Plain,
    Keyword,
    Function,
    Single,
    Double,
    Backtick,
    Number,
    Comment,
    Param,
}

fn is_keyword(w: &str) -> bool {
    let up = w.to_ascii_uppercase();
    KEYWORDS.binary_search(&up.as_str()).is_ok()
}

/// Byte ranges with their token class.
fn tokenize(s: &str) -> Vec<(usize, usize, Tok)> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        let c = b[i];
        let tok = if c == b'-' && b.get(i + 1) == Some(&b'-') || c == b'#' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            Tok::Comment
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                i += 1;
            }
            i = (i + 2).min(b.len());
            Tok::Comment
        } else if c == b'\'' || c == b'"' || c == b'`' {
            i += 1;
            while i < b.len() && b[i] != c {
                if b[i] == b'\\' && c == b'\'' {
                    i += 1;
                }
                i += 1;
            }
            i = (i + 1).min(b.len());
            match c {
                b'\'' => Tok::Single,
                b'"' => Tok::Double,
                _ => Tok::Backtick,
            }
        } else if c.is_ascii_digit() {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.') {
                i += 1;
            }
            Tok::Number
        } else if (c == b':' && b.get(i + 1).map(|n| n.is_ascii_alphabetic()).unwrap_or(false) && (i == 0 || b[i - 1] != b':')) || (c == b'$' && b.get(i + 1).map(|n| n.is_ascii_digit()).unwrap_or(false)) {
            i += 1;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            Tok::Param
        } else if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] >= 0x80) {
                i += 1;
            }
            let word = &s[start..i];
            let mut j = i;
            while j < b.len() && b[j] == b' ' {
                j += 1;
            }
            if is_keyword(word) {
                Tok::Keyword
            } else if b.get(j) == Some(&b'(') {
                Tok::Function
            } else {
                Tok::Plain
            }
        } else {
            // Advance by a whole UTF-8 char.
            i += s[i..].chars().next().map(|ch| ch.len_utf8()).unwrap_or(1);
            Tok::Plain
        };
        out.push((start, i, tok));
    }
    out
}

pub fn highlight(text: &str, font: FontId, current: Option<(usize, usize)>) -> LayoutJob {
    let pal = p();
    let mut job = LayoutJob::default();
    for (s, e, t) in tokenize(text) {
        let color = match t {
            Tok::Plain => pal.editor_text,
            Tok::Keyword => pal.editor_keyword,
            Tok::Function => pal.editor_function,
            Tok::Single => pal.editor_string,
            Tok::Double => pal.editor_dquote,
            Tok::Backtick => pal.editor_backtick,
            Tok::Number => pal.editor_number,
            Tok::Comment => pal.editor_comment,
            Tok::Param => pal.warning,
        };
        let background = match current {
            Some((cs, ce)) if s >= cs && e <= ce => pal.editor_current_query,
            _ => Color32::TRANSPARENT,
        };
        job.append(&text[s..e], 0.0, TextFormat { font_id: font.clone(), color, background, italics: t == Tok::Comment, ..Default::default() });
    }
    job
}

/// What the editor offers for completion: tables and their columns.
#[derive(Default, Clone)]
pub struct Catalog {
    pub tables: Vec<String>,
    pub columns: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub text: String,
    pub kind: &'static str,
}

struct Completion {
    start_char: usize,
    items: Vec<Suggestion>,
    selected: usize,
}

pub struct SqlEditor {
    pub id: egui::Id,
    completion: Option<Completion>,
    pub cursor_char: usize,
    pub selection_chars: Option<(usize, usize)>,
    pending_cursor: Option<(usize, usize)>,
    dismissed_at: Option<usize>,
}

#[derive(Default)]
pub struct EditorEvents {
    pub run_current: bool,
    pub run_all: bool,
    pub changed: bool,
}

fn char_to_byte(s: &str, ci: usize) -> usize {
    s.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(s.len())
}

fn byte_to_char(s: &str, bi: usize) -> usize {
    s[..bi.min(s.len())].chars().count()
}

/// Resolves `alias` to a table name using FROM/JOIN clauses in the text.
fn resolve_alias(text: &str, alias: &str) -> Option<String> {
    let re = regex::Regex::new(r#"(?i)\b(?:from|join|update|into)\s+([`"\[]?[\w.]+[`"\]]?)(?:\s+(?:as\s+)?(\w+))?"#).ok()?;
    for cap in re.captures_iter(text) {
        let table = cap.get(1)?.as_str().trim_matches(|c| c == '`' || c == '"' || c == '[' || c == ']');
        let short = table.rsplit('.').next().unwrap_or(table);
        let a = cap.get(2).map(|m| m.as_str());
        if a.map(|a| a.eq_ignore_ascii_case(alias)).unwrap_or(false) || short.eq_ignore_ascii_case(alias) {
            return Some(short.to_string());
        }
    }
    None
}

pub fn suggestions(text: &str, prefix: &str, catalog: &Catalog) -> Vec<Suggestion> {
    if let Some((qual, part)) = prefix.rsplit_once('.') {
        let table = resolve_alias(text, qual).unwrap_or_else(|| qual.to_string());
        let cols = catalog.columns.iter().find(|(t, _)| t.eq_ignore_ascii_case(&table)).map(|(_, c)| c.clone()).unwrap_or_default();
        return rank(cols.iter().map(|c| Suggestion { text: c.clone(), kind: "column" }), part);
    }
    let mentioned: Vec<String> = catalog.tables.iter().filter(|t| text.to_lowercase().contains(&t.to_lowercase())).cloned().collect();
    let mut pool: Vec<Suggestion> = Vec::new();
    for t in &mentioned {
        if let Some(cols) = catalog.columns.get(t) {
            pool.extend(cols.iter().map(|c| Suggestion { text: c.clone(), kind: "column" }));
        }
    }
    pool.extend(catalog.tables.iter().map(|t| Suggestion { text: t.clone(), kind: "table" }));
    pool.extend(KEYWORDS.iter().map(|k| Suggestion { text: k.to_string(), kind: "keyword" }));
    pool.extend(FUNCTIONS.iter().map(|k| Suggestion { text: format!("{k}()"), kind: "function" }));
    rank(pool.into_iter(), prefix)
}

fn rank(items: impl Iterator<Item = Suggestion>, prefix: &str) -> Vec<Suggestion> {
    let lp = prefix.to_lowercase();
    let mut starts = Vec::new();
    let mut contains = Vec::new();
    for s in items {
        let l = s.text.to_lowercase();
        if l == lp {
            continue;
        }
        if l.starts_with(&lp) {
            starts.push(s);
        } else if lp.len() >= 2 && l.contains(&lp) {
            contains.push(s);
        }
    }
    starts.extend(contains);
    let mut seen = std::collections::HashSet::new();
    starts.retain(|s| seen.insert(s.text.clone()));
    starts.truncate(40);
    starts
}

impl SqlEditor {
    pub fn new(id: egui::Id) -> Self {
        Self { id, completion: None, cursor_char: 0, selection_chars: None, pending_cursor: None, dismissed_at: None }
    }

    /// Byte range of the statement under the cursor.
    pub fn current_statement(&self, text: &str) -> Option<(usize, usize)> {
        let cursor = char_to_byte(text, self.cursor_char);
        let spans = crate::db::split_statements(text);
        spans.iter().copied().find(|(s, e)| cursor >= *s && cursor <= *e + 1).or_else(|| spans.iter().copied().filter(|(_, e)| *e < cursor).last())
    }

    pub fn selected_text<'a>(&self, text: &'a str) -> Option<&'a str> {
        let (a, b) = self.selection_chars?;
        let (a, b) = (char_to_byte(text, a.min(b)), char_to_byte(text, a.max(b)));
        let s = &text[a..b];
        (!s.trim().is_empty()).then_some(s)
    }

    fn toggle_comment(&mut self, text: &mut String) {
        let (a, b) = self.selection_chars.map(|(a, b)| (a.min(b), a.max(b))).unwrap_or((self.cursor_char, self.cursor_char));
        let (ab, bb) = (char_to_byte(text, a), char_to_byte(text, b));
        let line_start = text[..ab].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let line_end = text[bb..].find('\n').map(|i| bb + i).unwrap_or(text.len());
        let block = &text[line_start..line_end];
        let all_commented = block.lines().filter(|l| !l.trim().is_empty()).all(|l| l.trim_start().starts_with("--"));
        let new_block: Vec<String> = block
            .lines()
            .map(|l| {
                if all_commented {
                    let idx = l.find("--").unwrap_or(0);
                    let rest = &l[idx + 2..];
                    format!("{}{}", &l[..idx], rest.strip_prefix(' ').unwrap_or(rest))
                } else if l.trim().is_empty() {
                    l.to_string()
                } else {
                    format!("-- {l}")
                }
            })
            .collect();
        let replaced = new_block.join("\n");
        text.replace_range(line_start..line_end, &replaced);
        let end_char = byte_to_char(text, line_start + replaced.len());
        self.pending_cursor = Some((byte_to_char(text, line_start), end_char));
    }

    pub fn format(&mut self, text: &mut String, uppercase: bool, indent: usize) {
        let opts = sqlformat::FormatOptions { indent: sqlformat::Indent::Spaces(indent as u8), uppercase: Some(uppercase), lines_between_queries: 2, ..Default::default() };
        *text = sqlformat::format(text, &sqlformat::QueryParams::None, &opts);
    }

    pub fn show(&mut self, ui: &mut Ui, text: &mut String, catalog: &Catalog, font_size: f32, uppercase_keywords: bool, auto_close: bool) -> EditorEvents {
        let pal = p();
        let mut ev = EditorEvents::default();
        let font = FontId::monospace(font_size);
        let has_focus = ui.memory(|m| m.has_focus(self.id));

        if has_focus {
            ev.run_all = ui.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Enter));
            ev.run_current = ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Enter));
            if ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Slash)) {
                self.toggle_comment(text);
                ev.changed = true;
            }
        }

        // Completion navigation must be consumed before the TextEdit sees the keys.
        let mut accept: Option<Suggestion> = None;
        if has_focus {
            if let Some(c) = &mut self.completion {
                if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown)) {
                    c.selected = (c.selected + 1) % c.items.len().max(1);
                }
                if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowUp)) {
                    c.selected = (c.selected + c.items.len().max(1) - 1) % c.items.len().max(1);
                }
                if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Tab) || i.consume_key(Modifiers::NONE, Key::Enter)) {
                    accept = c.items.get(c.selected).cloned();
                }
                if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
                    self.dismissed_at = Some(self.cursor_char);
                    self.completion = None;
                }
            }
        }
        if let (Some(s), Some(c)) = (accept, self.completion.take()) {
            let start = char_to_byte(text, c.start_char);
            let end = char_to_byte(text, self.cursor_char);
            text.replace_range(start..end, &s.text);
            let mut caret = c.start_char + s.text.chars().count();
            if s.text.ends_with("()") {
                caret -= 1;
            }
            self.pending_cursor = Some((caret, caret));
            ev.changed = true;
        }

        let typed: Vec<String> = if has_focus {
            ui.input(|i| i.events.iter().filter_map(|e| if let egui::Event::Text(t) = e { Some(t.clone()) } else { None }).collect())
        } else {
            vec![]
        };

        if let Some((a, b)) = self.pending_cursor.take() {
            if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), self.id) {
                state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(a), CCursor::new(b))));
                state.store(ui.ctx(), self.id);
            }
            ui.memory_mut(|m| m.request_focus(self.id));
        }

        let current = self.current_statement(text);
        let mut layouter = |ui: &Ui, buf: &dyn egui::TextBuffer, _wrap: f32| {
            let job = highlight(buf.as_str(), font.clone(), current);
            ui.fonts_mut(|f| f.layout_job(job))
        };

        let line_count = text.lines().count().max(1) + usize::from(text.ends_with('\n'));
        let gutter_w = 14.0 + (line_count.to_string().len() as f32) * font_size * 0.62;
        let mut output = None;
        egui::ScrollArea::both().id_salt(self.id.with("scroll")).auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let (gutter, _) = ui.allocate_exact_size(egui::vec2(gutter_w, ui.available_height().max(10.0)), egui::Sense::hover());
                let te = egui::TextEdit::multiline(text)
                    .id(self.id)
                    .font(font.clone())
                    .code_editor()
                    .frame(egui::Frame::NONE)
                    .lock_focus(true)
                    .desired_width(f32::INFINITY)
                    .desired_rows(12)
                    .layouter(&mut layouter);
                let out = te.show(ui);
                let galley = out.galley.clone();
                let mut line = 1;
                let mut start_of_line = true;
                for row in &galley.rows {
                    if start_of_line {
                        let y = out.galley_pos.y + row.pos.y;
                        let is_current = current.is_some();
                        let _ = is_current;
                        ui.painter().text(egui::pos2(gutter.right() - 8.0, y), egui::Align2::RIGHT_TOP, line.to_string(), font.clone(), pal.text_faint);
                        line += 1;
                    }
                    start_of_line = row.ends_with_newline;
                }
                output = Some(out);
            });
        });
        let Some(out) = output else { return ev };

        if out.response.changed() {
            ev.changed = true;
        }
        if let Some(r) = out.cursor_range {
            self.cursor_char = r.primary.index.0;
            self.selection_chars = (r.primary.index.0 != r.secondary.index.0).then_some((r.secondary.index.0, r.primary.index.0));
        }

        if out.response.changed() && !typed.is_empty() {
            let last = typed.last().cloned().unwrap_or_default();
            if auto_close && self.selection_chars.is_none() {
                let close = match last.as_str() {
                    "(" => Some(")"),
                    "[" => Some("]"),
                    "{" => Some("}"),
                    "'" => Some("'"),
                    "\"" => Some("\""),
                    "`" => Some("`"),
                    _ => None,
                };
                let at = char_to_byte(text, self.cursor_char);
                let next = text[at..].chars().next();
                if let Some(cl) = close {
                    let quote = matches!(last.as_str(), "'" | "\"" | "`");
                    let prev_is_word = text[..at.saturating_sub(1)].chars().last().map(|c| c.is_alphanumeric()).unwrap_or(false);
                    if next.map(|n| n.to_string() == cl).unwrap_or(false) && quote {
                        // Typed over an auto-inserted closing quote: drop the duplicate.
                        text.remove(at);
                    } else if !(quote && prev_is_word) && next.map(|n| n.is_whitespace() || ")]},;".contains(n)).unwrap_or(true) {
                        text.insert_str(at, cl);
                        self.pending_cursor = Some((self.cursor_char, self.cursor_char));
                    }
                } else if matches!(last.as_str(), ")" | "]" | "}") && next.map(|n| n.to_string() == last).unwrap_or(false) {
                    text.remove(at);
                }
            }
            if uppercase_keywords && last.chars().all(|c| c.is_whitespace() || c == '(' || c == ',' || c == ';') {
                let at = char_to_byte(text, self.cursor_char.saturating_sub(1));
                let before = &text[..at];
                let word_start = before.rfind(|c: char| !(c.is_alphanumeric() || c == '_')).map(|i| i + 1).unwrap_or(0);
                let word = &before[word_start..];
                let in_string = tokenize(before).last().map(|t| matches!(t.2, Tok::Single | Tok::Double | Tok::Comment)).unwrap_or(false);
                if !word.is_empty() && is_keyword(word) && word != word.to_ascii_uppercase() && !in_string {
                    let up = word.to_ascii_uppercase();
                    text.replace_range(word_start..at, &up);
                }
            }
        }

        // Refresh completion list from the word before the caret.
        if has_focus && (out.response.changed() || self.completion.is_some()) {
            let at = char_to_byte(text, self.cursor_char);
            let before = &text[..at];
            let start = before.rfind(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.')).map(|i| i + 1).unwrap_or(0);
            let prefix = &before[start..];
            let in_string = tokenize(before).last().map(|t| matches!(t.2, Tok::Single | Tok::Double | Tok::Comment) && t.1 >= at).unwrap_or(false);
            let start_char = byte_to_char(text, start);
            if prefix.is_empty() || in_string || self.dismissed_at == Some(self.cursor_char) || prefix.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                self.completion = None;
            } else {
                let items = suggestions(text, prefix, catalog);
                let keep = self.completion.as_ref().map(|c| c.selected).unwrap_or(0);
                self.completion = (!items.is_empty()).then(|| Completion { start_char, selected: keep.min(items.len() - 1), items });
            }
        }
        if !has_focus {
            self.completion = None;
        }

        // Popup under the caret.
        if let Some(c) = &mut self.completion {
            let caret = out.galley.pos_from_cursor(CCursor::new(self.cursor_char));
            let anchor = out.galley_pos + caret.left_bottom().to_vec2() + egui::vec2(0.0, 4.0);
            let mut clicked = None;
            egui::Area::new(self.id.with("completion")).order(egui::Order::Tooltip).fixed_pos(anchor).show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
                    ui.set_width(300.0);
                    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                        for (i, s) in c.items.iter().enumerate() {
                            let selected = i == c.selected;
                            let resp = ui.horizontal(|ui| {
                                let color = match s.kind {
                                    "keyword" => pal.editor_keyword,
                                    "table" => pal.accent,
                                    "column" => pal.success,
                                    _ => pal.editor_function,
                                };
                                let r = ui.add_sized([210.0, 20.0], egui::Button::selectable(selected, egui::RichText::new(&s.text).monospace().size(12.5)));
                                ui.label(egui::RichText::new(s.kind).size(10.5).color(color));
                                r
                            });
                            if selected {
                                resp.inner.scroll_to_me(None);
                            }
                            if resp.inner.clicked() {
                                clicked = Some(i);
                            }
                        }
                    });
                });
            });
            if let Some(i) = clicked {
                c.selected = i;
                let s = c.items[i].clone();
                let start = char_to_byte(text, c.start_char);
                let end = char_to_byte(text, self.cursor_char);
                text.replace_range(start..end, &s.text);
                let caret = c.start_char + s.text.chars().count();
                self.pending_cursor = Some((caret, caret));
                self.completion = None;
                ev.changed = true;
            }
        }
        ev
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_are_sorted_for_binary_search() {
        let mut sorted = KEYWORDS.to_vec();
        sorted.sort();
        assert_eq!(sorted, KEYWORDS.to_vec());
    }

    #[test]
    fn tokenizer_classifies_sql() {
        let s = "SELECT count(*), 'x' FROM t -- note\nWHERE id = :id";
        let toks: Vec<(&str, Tok)> = tokenize(s).into_iter().filter(|t| t.2 != Tok::Plain).map(|(a, b, t)| (&s[a..b], t)).collect();
        assert!(toks.contains(&("SELECT", Tok::Keyword)));
        assert!(toks.contains(&("count", Tok::Function)));
        assert!(toks.contains(&("'x'", Tok::Single)));
        assert!(toks.contains(&("-- note", Tok::Comment)));
        assert!(toks.contains(&(":id", Tok::Param)));
    }

    #[test]
    fn postgres_casts_are_not_params() {
        let s = "select a::text";
        assert!(!tokenize(s).iter().any(|t| t.2 == Tok::Param));
    }

    #[test]
    fn alias_completion_resolves_columns() {
        let mut cat = Catalog::default();
        cat.tables = vec!["users".into(), "orders".into()];
        cat.columns.insert("users".into(), vec!["id".into(), "email".into()]);
        let text = "select u. from public.users u";
        let s = suggestions(text, "u.em", &cat);
        assert_eq!(s.first().map(|s| s.text.as_str()), Some("email"));
    }

    #[test]
    fn prefix_matches_rank_before_substring_matches() {
        let cat = Catalog { tables: vec!["user_roles".into(), "app_users".into()], ..Default::default() };
        let s = suggestions("", "user", &cat);
        assert_eq!(s[0].text, "user_roles");
        assert!(s.iter().any(|x| x.text == "app_users"));
    }
}
