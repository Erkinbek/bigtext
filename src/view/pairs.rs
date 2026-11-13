//! Brackets and quotes in code, as code editors do: an opening bracket or quote typed gets its
//! partner after the caret, a selection is wrapped, typing the closing one over its partner
//! steps over it, Backspace between an empty pair removes both, and Enter between `{}` (or
//! `[]`, `()`) opens an indented line between them. Only for one caret; other text is typed as
//! it is.

use crate::app::document::{Document, EditKind};
use crate::view::highlight::Lang;

/// Selections larger than this are not wrapped (they are replaced, as usual).
const WRAP_MAX: u64 = 1024 * 1024;

fn partner(c: char) -> Option<char> {
    Some(match c {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '"' => '"',
        '\'' => '\'',
        '`' => '`',
        _ => return None,
    })
}

fn is_quote(c: char) -> bool {
    matches!(c, '"' | '\'' | '`')
}

/// Brackets and quotes pair themselves in this language.
pub fn enabled(lang: Lang) -> bool {
    lang.is_code() || matches!(lang, Lang::Json)
}

/// The quotes that pair themselves in `lang` (a Rust `'` is often a lifetime).
fn pairs_quote(lang: Lang, q: char) -> bool {
    match q {
        '\'' => !matches!(lang, Lang::Rust),
        '`' => matches!(
            lang,
            Lang::JavaScript | Lang::TypeScript | Lang::Go | Lang::Shell
        ),
        _ => true,
    }
}

fn char_after(doc: &Document, off: u64) -> Option<char> {
    if off >= doc.buf.len() {
        return None;
    }
    let b = doc.buf.read(off..doc.buf.next_char(off))?;
    Some(doc.buf.codec.decode_char(&b).0)
}

fn char_before(doc: &Document, off: u64) -> Option<char> {
    if off <= doc.min_caret() {
        return None;
    }
    let p = doc.buf.prev_char(off);
    let b = doc.buf.read(p..off)?;
    Some(doc.buf.codec.decode_char(&b).0)
}

/// After the caret a pair may open: the line ends, or white space or a closing character
/// follows.
fn free_after(c: Option<char>) -> bool {
    c.is_none_or(|c| c.is_whitespace() || matches!(c, ')' | ']' | '}' | ',' | ';' | ':'))
}

/// `t` typed with one caret: true if it was handled here.
pub fn typed(doc: &mut Document, t: &str, lang: Lang) -> bool {
    let mut chars = t.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return false;
    };
    if !enabled(lang) || !doc.editable() {
        return false;
    }
    let (s, e) = doc.selection();
    let codec = doc.buf.codec;
    let close = partner(c);
    // A selection is wrapped, and stays selected inside the pair.
    if s < e {
        let Some(close) = close.filter(|_| e - s <= WRAP_MAX) else {
            return false;
        };
        if is_quote(c) && !pairs_quote(lang, c) {
            return false;
        }
        let Some(inner) = doc.buf.read(s..e) else {
            return false;
        };
        let open = codec.encode(&c.to_string());
        let mut bytes = open.clone();
        bytes.extend_from_slice(&inner);
        bytes.extend(codec.encode(&close.to_string()));
        doc.edit(s..e, &bytes, EditKind::Other);
        let o = open.len() as u64;
        doc.select(s + o, e + o);
        return true;
    }
    let next = char_after(doc, s);
    // The closing character over its partner: step over it.
    if (matches!(c, ')' | ']' | '}') || is_quote(c))
        && next == Some(c)
        && (!is_quote(c) || pairs_quote(lang, c))
    {
        doc.set_caret(doc.buf.next_char(s), false);
        return true;
    }
    let Some(close) = close else { return false };
    if !free_after(next) {
        return false;
    }
    if is_quote(c) {
        let prev = char_before(doc, s);
        // Not after a letter (don't), a backslash or another quote.
        if !pairs_quote(lang, c)
            || prev.is_some_and(|p| p.is_alphanumeric() || p == '_' || p == '\\' || is_quote(p))
        {
            return false;
        }
    }
    let mut bytes = codec.encode(&c.to_string());
    let first = bytes.len() as u64;
    bytes.extend(codec.encode(&close.to_string()));
    doc.edit(s..s, &bytes, EditKind::Typing);
    doc.set_caret(s + first, false);
    true
}

/// Backspace with one caret between an empty pair: removes both.
pub fn backspace(doc: &mut Document, lang: Lang) -> bool {
    if !enabled(lang) || !doc.editable() || doc.has_selection() {
        return false;
    }
    let c = doc.view.caret;
    let (Some(p), Some(n)) = (char_before(doc, c), char_after(doc, c)) else {
        return false;
    };
    if partner(p) != Some(n) || (is_quote(p) && !pairs_quote(lang, p)) {
        return false;
    }
    let (from, to) = (doc.buf.prev_char(c), doc.buf.next_char(c));
    doc.edit(from..to, &[], EditKind::Delete);
    true
}

/// Enter with one caret: between `{}`, `[]` or `()` the closing one goes to a line of its own
/// below an indented empty line; after an opening bracket (or a Python or YAML `:`) the new line is
/// indented one step more.
pub fn enter(doc: &mut Document, lang: Lang, unit: &str) -> bool {
    if !enabled(lang) || !doc.editable() || doc.has_selection() {
        return false;
    }
    let c = doc.view.caret;
    let prev = char_before(doc, c);
    let opens = prev.is_some_and(|p| matches!(p, '{' | '[' | '('))
        || (matches!(lang, Lang::Python | Lang::Yaml) && prev == Some(':'));
    if !opens {
        return false;
    }
    let codec = doc.buf.codec;
    let indent = doc.indent_of_line(c);
    let nl = codec.line_break(doc.line_ending);
    let tab = codec.encode("\t");
    let step = if indent.starts_with(&tab) {
        tab
    } else {
        codec.encode(unit)
    };
    let mut bytes = nl.clone();
    bytes.extend_from_slice(&indent);
    bytes.extend_from_slice(&step);
    let caret = c + bytes.len() as u64;
    if prev.and_then(partner) == char_after(doc, c) && prev != Some(':') {
        bytes.extend_from_slice(&nl);
        bytes.extend_from_slice(&indent);
    }
    doc.edit(c..c, &bytes, EditKind::Other);
    doc.set_caret(caret, false);
    true
}
