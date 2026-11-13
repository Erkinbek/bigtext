//! Formats that are not programming languages but are written like code: CSS (with SCSS and
//! Less), Markdown, YAML, Dockerfiles and Makefiles. Each has its own small tokenizer; what
//! spans lines (a CSS comment, a Markdown code fence, a YAML block scalar, a Dockerfile line
//! continued with `\`) is carried in [`State`].

use super::{is_ident, number_end, string_end, word_end, Kind, Lang, Mode, Sink, State};

/// Colours `t[from..]` as shell commands (Dockerfile `RUN`, Makefile recipes).
fn shell(t: &[u8], from: usize, out: &mut Sink) {
    if out.0.is_none() || from >= t.len() {
        return;
    }
    let mut v = Vec::new();
    let mut st = State::default();
    super::code::spans(Lang::Shell, &mut st, &t[from..], &mut Sink(Some(&mut v)));
    for (a, b, k) in v {
        out.push(a + from, b + from, k);
    }
}

fn lead(t: &[u8]) -> usize {
    t.iter().take_while(|&&b| b == b' ' || b == b'\t').count()
}

fn skip_space(t: &[u8], mut i: usize) -> usize {
    while i < t.len() && (t[i] == b' ' || t[i] == b'\t') {
        i += 1;
    }
    i
}

// ---------------------------------------------------------------------------------------------
// CSS, SCSS, Less

/// A CSS name: letters, digits, `-` and `_`.
fn css_word_end(t: &[u8], mut i: usize) -> usize {
    while i < t.len() && (is_ident(t[i]) || t[i] == b'-') {
        i += 1;
    }
    i
}

/// `State::depth` counts the open braces: inside one, `name:` is a property.
pub(super) fn css(st: &mut State, t: &[u8], out: &mut Sink) {
    let mut i = 0;
    // After a property's `:`, up to `;` or `}`.
    let mut value = false;
    // Inside `( )`: `@media (max-width: 600px)` has a property too.
    let mut paren = 0u32;
    while i < t.len() {
        if let Mode::Comment(_) = st.mode {
            let (e, closed) = super::close(t, i, b"*/");
            out.push(i, e, Kind::Comment);
            i = e;
            if closed {
                st.mode = Mode::Code;
            }
            continue;
        }
        let c = t[i];
        match c {
            b' ' | b'\t' | b'\r' => i += 1,
            b'/' if t.get(i + 1) == Some(&b'*') => {
                st.mode = Mode::Comment(1);
                let (e, closed) = super::close(t, i + 2, b"*/");
                out.push(i, e, Kind::Comment);
                i = e;
                if closed {
                    st.mode = Mode::Code;
                }
            }
            // SCSS and Less line comments (not `url(http://…)`, which is taken whole below).
            b'/' if t.get(i + 1) == Some(&b'/')
                && (i == 0 || matches!(t[i - 1], b' ' | b'\t' | b';' | b'{' | b'}')) =>
            {
                out.push(i, t.len(), Kind::Comment);
                return;
            }
            b'"' | b'\'' => {
                let e = string_end(t, i, c, true);
                out.push(i, e, Kind::Str);
                i = e;
            }
            b'{' => {
                st.depth = st.depth.saturating_add(1);
                value = false;
                i += 1;
            }
            b'}' => {
                st.depth = st.depth.saturating_sub(1);
                value = false;
                i += 1;
            }
            b';' => {
                value = false;
                i += 1;
            }
            b'(' => {
                paren += 1;
                i += 1;
            }
            b')' => {
                paren = paren.saturating_sub(1);
                if paren == 0 && st.depth == 0 {
                    value = false;
                }
                i += 1;
            }
            b'!' if t[i + 1..].starts_with(b"important") => {
                out.push(i, i + 10, Kind::Keyword);
                i += 10;
            }
            // @media, @import, @include; a Less variable `@name: value`.
            b'@' if t.get(i + 1).is_some_and(|&b| is_ident(b)) => {
                let e = css_word_end(t, i + 1);
                let var = t.get(skip_space(t, e)) == Some(&b':');
                out.push(i, e, if var { Kind::Var } else { Kind::Keyword });
                i = e;
            }
            // SCSS variables.
            b'$' if t.get(i + 1).is_some_and(|&b| is_ident(b)) => {
                let e = css_word_end(t, i + 1);
                out.push(i, e, Kind::Var);
                i = e;
            }
            // A colour in a value; an id in a selector.
            b'#' if t.get(i + 1).is_some_and(|&b| is_ident(b) || b == b'-') => {
                let e = css_word_end(t, i + 1);
                let hex = t[i + 1..e].iter().all(u8::is_ascii_hexdigit)
                    && matches!(e - i - 1, 3 | 4 | 6 | 8);
                out.push(
                    i,
                    e,
                    if value && hex {
                        Kind::Number
                    } else {
                        Kind::Attr
                    },
                );
                i = e;
            }
            b'.' if !value && t.get(i + 1).is_some_and(|&b| is_ident(b) || b == b'-') => {
                let e = css_word_end(t, i + 1);
                out.push(i, e, Kind::Attr);
                i = e;
            }
            // :hover, ::before in a selector.
            b':' if !value && t.get(i + 1).is_some_and(|&b| b == b':' || is_ident(b)) => {
                let s = i + 1 + (t[i + 1] == b':') as usize;
                let e = css_word_end(t, s);
                out.push(i, e, Kind::Decorator);
                i = e;
            }
            b':' => {
                value = true;
                i += 1;
            }
            b'&' => {
                out.push(i, i + 1, Kind::Keyword);
                i += 1;
            }
            _ if c.is_ascii_digit()
                || (c == b'.' && t.get(i + 1).is_some_and(u8::is_ascii_digit))
                || (c == b'-' && t.get(i + 1).is_some_and(u8::is_ascii_digit)) =>
            {
                let mut e = i + 1;
                while e < t.len() && (t[e].is_ascii_alphanumeric() || t[e] == b'.' || t[e] == b'%')
                {
                    e += 1;
                }
                out.push(i, e, Kind::Number);
                i = e;
            }
            _ if is_ident(c) || c == b'-' => {
                let e = css_word_end(t, i);
                let w = &t[i..e];
                let next = skip_space(t, e);
                if w.eq_ignore_ascii_case(b"url") && t.get(e) == Some(&b'(') {
                    let close = t[e..]
                        .iter()
                        .position(|&b| b == b')')
                        .map_or(t.len(), |p| e + p + 1);
                    out.push(i, e, Kind::Function);
                    out.push(e + 1, close.saturating_sub(1).max(e + 1), Kind::Str);
                    i = close;
                    continue;
                }
                if t.get(e) == Some(&b'(') {
                    out.push(i, e, Kind::Function);
                } else if value {
                    // Values (`solid`, `auto`) stay plain.
                } else if (st.depth > 0 || paren > 0)
                    && t.get(next) == Some(&b':')
                    && is_property(t, next)
                {
                    out.push(i, e, Kind::Key);
                } else {
                    out.push(i, e, Kind::Tag);
                }
                i = e;
            }
            _ => i += 1,
        }
    }
}

/// `name:` at `colon` is a property, not a selector with a pseudo-class (`a:hover {`): a space
/// follows the colon, or the line opens no block.
fn is_property(t: &[u8], colon: usize) -> bool {
    matches!(t.get(colon + 1), None | Some(b' ' | b'\t')) || !t[colon..].contains(&b'{')
}

// ---------------------------------------------------------------------------------------------
// Markdown

/// Headings and list markers red, bold orange, italics purple, code green, links blue with
/// cyan addresses, quotes grey. Fenced code blocks (``` or ~~~) are carried in the state.
pub(super) fn markdown(st: &mut State, t: &[u8], out: &mut Sink) {
    let l = lead(t);
    let body = &t[l..];
    if st.mode == Mode::Delim {
        // Inside a fence: closed by a line starting with the same marker.
        if body.starts_with(st.delim()) {
            out.push(l, t.len(), Kind::Comment);
            st.mode = Mode::Code;
        } else {
            out.push(0, t.len(), Kind::Str);
        }
        return;
    }
    let mut i = 0;
    if let Mode::Comment(_) = st.mode {
        let (e, closed) = super::close(t, 0, b"-->");
        out.push(0, e, Kind::Comment);
        if !closed {
            return;
        }
        st.mode = Mode::Code;
        i = e;
    }
    if i == 0 {
        for marker in [&b"```"[..], &b"~~~"[..]] {
            if body.starts_with(marker) {
                let n = body.iter().take_while(|&&b| b == marker[0]).count();
                st.set_delim(&body[..n.min(super::DELIM)]);
                st.mode = Mode::Delim;
                out.push(l, t.len(), Kind::Comment);
                return;
            }
        }
        let hashes = body.iter().take_while(|&&b| b == b'#').count();
        if (1..=6).contains(&hashes) && matches!(body.get(hashes), None | Some(b' ' | b'\t')) {
            out.push(l, t.len(), Kind::Var);
            return;
        }
        // A rule or a setext underline: ---, ***, ===.
        if body.len() >= 3
            && body
                .iter()
                .all(|&b| matches!(b, b'-' | b'*' | b'_' | b'=' | b' '))
            && body.iter().filter(|&&b| b != b' ').count() >= 3
        {
            out.push(l, t.len(), Kind::Var);
            return;
        }
        if body.starts_with(b">") {
            out.push(l, t.len(), Kind::Comment);
            return;
        }
        let digits = body.iter().take_while(|b| b.is_ascii_digit()).count();
        let marker = if matches!(body.first(), Some(b'-' | b'*' | b'+'))
            && matches!(body.get(1), Some(b' ' | b'\t'))
        {
            1
        } else if digits > 0
            && matches!(body.get(digits), Some(b'.' | b')'))
            && matches!(body.get(digits + 1), Some(b' ' | b'\t'))
        {
            digits + 1
        } else {
            0
        };
        if marker > 0 {
            out.push(l, l + marker, Kind::Var);
            i = l + marker;
        }
    }
    md_inline(st, t, i, out);
}

fn md_inline(st: &mut State, t: &[u8], mut i: usize, out: &mut Sink) {
    let boundary = |j: usize| j == 0 || !t[j - 1].is_ascii_alphanumeric();
    while i < t.len() {
        let c = t[i];
        match c {
            b'`' => {
                let n = t[i..].iter().take_while(|&&b| b == b'`').count();
                let fence = &t[i..i + n];
                let e = super::close(t, i + n, fence).0;
                out.push(i, e, Kind::Str);
                i = e;
            }
            b'<' if t[i..].starts_with(b"<!--") => {
                let (e, closed) = super::close(t, i + 4, b"-->");
                out.push(i, e, Kind::Comment);
                if !closed {
                    st.mode = Mode::Comment(1);
                    return;
                }
                i = e;
            }
            // <https://autolink>
            b'<' if t[i + 1..].starts_with(b"http") => {
                let e = t[i..]
                    .iter()
                    .position(|&b| b == b'>')
                    .map_or(t.len(), |p| i + p + 1);
                out.push(i, e, Kind::Decorator);
                i = e;
            }
            // [text](address), ![alt](address)
            b'[' | b'!' if c == b'[' || t.get(i + 1) == Some(&b'[') => {
                let s = i + (c == b'!') as usize;
                let Some(close) = t[s..].iter().position(|&b| b == b']').map(|p| s + p) else {
                    i += 1;
                    continue;
                };
                out.push(i, close + 1, Kind::Function);
                i = close + 1;
                if t.get(i) == Some(&b'(') {
                    let e = t[i..]
                        .iter()
                        .position(|&b| b == b')')
                        .map_or(t.len(), |p| i + p + 1);
                    out.push(i, e, Kind::Decorator);
                    i = e;
                }
            }
            b'*' | b'_' if boundary(i) || c == b'*' => {
                let n = t[i..].iter().take_while(|&&b| b == c).count().min(3);
                let marker = &t[i..i + n];
                // The closing marker, not right after a space and (for `_`) at the end of a word.
                let mut j = i + n;
                let mut found = None;
                while let Some(p) = memchr::memmem::find(&t[j..], marker) {
                    let at = j + p;
                    if at > i + n
                        && t[at - 1] != b' '
                        && (c == b'*' || t.get(at + n).is_none_or(|b| !b.is_ascii_alphanumeric()))
                    {
                        found = Some(at);
                        break;
                    }
                    j = at + 1;
                }
                match found {
                    Some(at) if t.get(i + n).is_some_and(|&b| b != b' ') => {
                        out.push(i, at + n, if n >= 2 { Kind::Number } else { Kind::Keyword });
                        i = at + n;
                    }
                    _ => i += n,
                }
            }
            _ => i += 1,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// YAML

/// Keys blue, plain values as strings, numbers and `true`/`null` as constants, `&anchors` and
/// `*aliases` red, `!tags` cyan. After `key: |` (or `>`), the more indented lines below are text:
/// `State::depth` holds the key's indentation plus one.
pub(super) fn yaml(st: &mut State, t: &[u8], out: &mut Sink) {
    let l = lead(t);
    if st.mode == Mode::Block {
        if l == t.len() || l >= st.depth as usize {
            out.push(0, t.len(), Kind::Str);
            return;
        }
        st.mode = Mode::Code;
    }
    let body = &t[l..];
    if body.starts_with(b"#") {
        out.push(l, t.len(), Kind::Comment);
        return;
    }
    if l == 0 && (body.starts_with(b"---") || body.starts_with(b"...")) {
        out.push(0, 3, Kind::Keyword);
        yaml_value(st, t, 3, l, out);
        return;
    }
    let mut i = l;
    // List items: `- `.
    while t.get(i) == Some(&b'-') && matches!(t.get(i + 1), None | Some(b' ' | b'\t')) {
        out.push(i, i + 1, Kind::Punct);
        i = skip_space(t, i + 1);
    }
    // key: (a quoted key too)
    let key_end = if matches!(t.get(i), Some(b'"' | b'\'')) {
        let e = string_end(t, i, t[i], t[i] == b'"');
        (t.get(skip_space(t, e)) == Some(&b':')).then_some(e)
    } else {
        let mut j = i;
        let mut found = None;
        while j < t.len() {
            if t[j] == b':' && matches!(t.get(j + 1), None | Some(b' ' | b'\t')) {
                found = Some(j);
                break;
            }
            if t[j] == b'#' && j > 0 && t[j - 1] == b' ' {
                break;
            }
            j += 1;
        }
        found.filter(|&e| {
            e > i
                && !matches!(
                    t[i],
                    b'[' | b'{' | b'"' | b'\'' | b'&' | b'*' | b'!' | b'|' | b'>'
                )
        })
    };
    if let Some(e) = key_end {
        let e = if matches!(t[i], b'"' | b'\'') {
            e
        } else {
            t[..e]
                .iter()
                .rposition(|&b| b != b' ' && b != b'\t')
                .map_or(e, |p| p + 1)
        };
        out.push(i, e, Kind::Key);
        let colon = t[e..]
            .iter()
            .position(|&b| b == b':')
            .map_or(e, |p| e + p + 1);
        yaml_value(st, t, colon, l, out);
    } else {
        yaml_value(st, t, i, l, out);
    }
}

fn yaml_value(st: &mut State, t: &[u8], from: usize, indent: usize, out: &mut Sink) {
    let mut i = skip_space(t, from);
    while i < t.len() {
        let c = t[i];
        match c {
            b' ' | b'\t' | b',' | b'[' | b']' | b'{' | b'}' => i += 1,
            b'#' if i == 0 || matches!(t[i - 1], b' ' | b'\t') => {
                out.push(i, t.len(), Kind::Comment);
                return;
            }
            b'"' | b'\'' => {
                let e = string_end(t, i, c, c == b'"');
                out.push(i, e, Kind::Str);
                i = e;
            }
            b'&' | b'*' if t.get(i + 1).is_some_and(|&b| is_ident(b)) => {
                let e = css_word_end(t, i + 1);
                out.push(i, e, Kind::Var);
                i = e;
            }
            b'!' => {
                let e = t[i..]
                    .iter()
                    .position(|&b| b == b' ')
                    .map_or(t.len(), |p| i + p);
                out.push(i, e, Kind::Decorator);
                i = e;
            }
            // A block scalar: the more indented lines below are its text.
            b'|' | b'>' if t[i + 1..].iter().all(|b| b"+-0123456789 \t\r".contains(b)) => {
                out.push(i, t.len(), Kind::Keyword);
                st.mode = Mode::Block;
                st.depth = (indent + 1).min(255) as u8;
                return;
            }
            _ => {
                // A plain scalar up to ` #`, or `,` / `]` / `}` in a flow collection.
                let flow = t[from..i].iter().any(|&b| b == b'[' || b == b'{');
                let mut e = i;
                while e < t.len()
                    && !(t[e] == b'#' && t[e - 1] == b' ')
                    && !(flow && matches!(t[e], b',' | b']' | b'}'))
                {
                    e += 1;
                }
                let end = t[..e]
                    .iter()
                    .rposition(|&b| b != b' ' && b != b'\t' && b != b'\r')
                    .map_or(i, |p| p + 1)
                    .max(i);
                let w = &t[i..end];
                let constant = matches!(
                    w,
                    b"true"
                        | b"false"
                        | b"True"
                        | b"False"
                        | b"TRUE"
                        | b"FALSE"
                        | b"yes"
                        | b"no"
                        | b"on"
                        | b"off"
                        | b"null"
                        | b"Null"
                        | b"NULL"
                        | b"~"
                );
                let number = (w[0].is_ascii_digit()
                    || (w.len() > 1 && matches!(w[0], b'-' | b'+' | b'.')))
                    && number_end(w, 1) == w.len();
                if end > i {
                    out.push(
                        i,
                        end,
                        if constant || number {
                            Kind::Number
                        } else {
                            Kind::Str
                        },
                    );
                }
                i = e.max(i + 1);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Dockerfile

const DOCKER: &[&str] = &[
    "add",
    "arg",
    "cmd",
    "copy",
    "entrypoint",
    "env",
    "expose",
    "from",
    "healthcheck",
    "label",
    "maintainer",
    "onbuild",
    "run",
    "shell",
    "stopsignal",
    "user",
    "volume",
    "workdir",
];

/// Instructions as keywords, the rest as shell commands; `State::depth` is 1 on a line that goes
/// on from a line ending with `\`.
pub(super) fn dockerfile(st: &mut State, t: &[u8], out: &mut Sink) {
    let continued = st.depth == 1;
    st.depth = u8::from(t.trim_ascii_end().ends_with(b"\\"));
    let l = lead(t);
    if t[l..].starts_with(b"#") && !continued {
        out.push(l, t.len(), Kind::Comment);
        st.depth = 0;
        return;
    }
    let mut from = l;
    if !continued {
        let e = word_end(t, l);
        let w = t[l..e].to_ascii_lowercase();
        if DOCKER.binary_search_by(|k| k.as_bytes().cmp(&w)).is_ok() {
            out.push(l, e, Kind::Keyword);
            from = e;
            // FROM image AS name
            if w == b"from" {
                if let Some(p) = t.windows(4).position(|x| x.eq_ignore_ascii_case(b" as ")) {
                    out.push(p + 1, p + 3, Kind::Keyword);
                    let s = skip_space(t, p + 4);
                    out.push(s, word_end(t, s), Kind::Type);
                    return;
                }
            }
        }
    }
    shell(t, from, out);
}

// ---------------------------------------------------------------------------------------------
// Makefile

const MAKE: &[&str] = &[
    "-include", "define", "else", "endef", "endif", "export", "ifdef", "ifeq", "ifndef", "ifneq",
    "include", "override", "sinclude", "undefine", "unexport", "vpath",
];

/// `$(…)`, `${…}` and `$@` from `i`: their end.
fn make_var(t: &[u8], i: usize) -> Option<usize> {
    match t.get(i + 1)? {
        b'(' | b'{' => {
            let (open, close) = if t[i + 1] == b'(' {
                (b'(', b')')
            } else {
                (b'{', b'}')
            };
            let mut depth = 0;
            for (j, &b) in t.iter().enumerate().skip(i + 1) {
                if b == open {
                    depth += 1;
                } else if b == close {
                    depth -= 1;
                    if depth == 0 {
                        return Some(j + 1);
                    }
                }
            }
            Some(t.len())
        }
        _ => Some(i + 2),
    }
}

/// Targets as functions, variables red, directives as keywords, recipes (lines starting with a
/// tab) as shell commands.
pub(super) fn makefile(t: &[u8], out: &mut Sink) {
    if t.first() == Some(&b'\t') {
        shell(t, 1, out);
        let mut i = 1;
        while let Some(p) = memchr::memchr(b'$', &t[i..]) {
            let at = i + p;
            let e = make_var(t, at).unwrap_or(at + 1);
            out.push(at, e, Kind::Var);
            i = e.max(at + 1);
        }
        return;
    }
    let l = lead(t);
    if t[l..].starts_with(b"#") {
        out.push(l, t.len(), Kind::Comment);
        return;
    }
    let e = l + t[l..]
        .iter()
        .take_while(|&&b| !matches!(b, b' ' | b'\t' | b':' | b'=' | b'?' | b'+' | b'!' | b'#'))
        .count();
    let word = &t[l..e];
    let mut i = e;
    if MAKE.iter().any(|k| k.as_bytes() == word) {
        out.push(l, e, Kind::Keyword);
    } else {
        let after = skip_space(t, e);
        let rest = &t[after..];
        let assign = rest.starts_with(b"=")
            || rest.starts_with(b":=")
            || rest.starts_with(b"::=")
            || rest.starts_with(b"?=")
            || rest.starts_with(b"+=")
            || rest.starts_with(b"!=");
        if assign && e > l {
            out.push(l, e, Kind::Var);
        } else if let Some(colon) = t[l..]
            .iter()
            .position(|&b| b == b':')
            .map(|p| l + p)
            .filter(|&c| t.get(c + 1) != Some(&b'='))
        {
            // targets: prerequisites
            let targets = &t[l..colon];
            if !targets.iter().all(|b| b.is_ascii_whitespace())
                && memchr::memchr(b'=', targets).is_none()
            {
                out.push(
                    l,
                    colon,
                    if targets.starts_with(b".") {
                        Kind::Keyword
                    } else {
                        Kind::Function
                    },
                );
                i = colon + 1;
            }
        }
    }
    while i < t.len() {
        match t[i] {
            b'$' => {
                let e = make_var(t, i).unwrap_or(i + 1);
                out.push(i, e, Kind::Var);
                i = e.max(i + 1);
            }
            b'#' => {
                out.push(i, t.len(), Kind::Comment);
                return;
            }
            b'"' | b'\'' => {
                let e = string_end(t, i, t[i], true);
                out.push(i, e, Kind::Str);
                i = e;
            }
            _ => i += 1,
        }
    }
}
