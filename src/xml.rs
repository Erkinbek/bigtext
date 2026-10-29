//! Streaming XML for files of any size, for the structure panel: the child elements of an
//! element, page by page; the element at a path such as `/catalog/item[1234]/price` (1-based,
//! like XPath); and the path of the element at an offset. Elements are skipped with a byte loop
//! over tags, comments, CDATA and processing instructions, so nothing is held as a tree. UTF-8
//! (or another ASCII-compatible encoding) only.

use crate::jobs::CancelToken;
use crate::json::{Child, Kind, Page, Resume};
use crate::text::TextBuf;

/// Bytes read at a time.
const CHUNK: u64 = 1 << 20;
/// Characters of a start tag shown as a node's label.
const LABEL_MAX: usize = 100;

struct Src<'a> {
    buf: &'a TextBuf,
    data: Vec<u8>,
    start: u64,
    i: usize,
    cancel: &'a CancelToken,
}

type Res<T> = Result<T, String>;

impl<'a> Src<'a> {
    fn new(buf: &'a TextBuf, at: u64, cancel: &'a CancelToken) -> Src<'a> {
        Src { buf, data: Vec::new(), start: at, i: 0, cancel }
    }

    fn pos(&self) -> u64 {
        self.start + self.i as u64
    }

    fn fill(&mut self) -> Res<bool> {
        if self.i < self.data.len() {
            return Ok(true);
        }
        if self.cancel.is_cancelled() {
            return Err("cancelled".into());
        }
        let at = self.pos();
        if at >= self.buf.len() {
            return Ok(false);
        }
        self.data = self.buf.read(at..(at + CHUNK).min(self.buf.len())).ok_or("could not read the file")?.to_vec();
        self.start = at;
        self.i = 0;
        Ok(true)
    }

    fn peek(&mut self) -> Res<Option<u8>> {
        Ok(if self.fill()? { Some(self.data[self.i]) } else { None })
    }

    /// Whether the text at the position starts with `s` (looking at most `s.len()` bytes ahead).
    fn starts(&mut self, s: &[u8]) -> Res<bool> {
        if !self.fill()? {
            return Ok(false);
        }
        if self.data.len() - self.i >= s.len() {
            return Ok(self.data[self.i..].starts_with(s));
        }
        let at = self.pos();
        Ok(self.buf.read(at..(at + s.len() as u64).min(self.buf.len())).is_some_and(|b| &b[..] == s))
    }

    /// Moves to the next `<`.
    fn seek_lt(&mut self) -> Res<bool> {
        loop {
            if !self.fill()? {
                return Ok(false);
            }
            match memchr::memchr(b'<', &self.data[self.i..]) {
                Some(k) => {
                    self.i += k;
                    return Ok(true);
                }
                None => self.i = self.data.len(),
            }
        }
    }

    /// Moves past the next `end` (a short marker like `-->`).
    fn past(&mut self, end: &[u8]) -> Res<()> {
        loop {
            if !self.fill()? {
                return Err(format!("{} is missing at the end of the file", String::from_utf8_lossy(end)));
            }
            if self.starts(end)? {
                self.i += end.len();
                return Ok(());
            }
            match memchr::memchr(end[0], &self.data[self.i + 1..]) {
                Some(k) => self.i += k + 1,
                None => self.i = self.data.len(),
            }
        }
    }

    /// Skips a comment, CDATA, processing instruction or declaration at `<`; false when the
    /// `<` starts a tag.
    fn skip_markup(&mut self) -> Res<bool> {
        if self.starts(b"<!--")? {
            self.i += 4;
            self.past(b"-->")?;
        } else if self.starts(b"<![CDATA[")? {
            self.i += 9;
            self.past(b"]]>")?;
        } else if self.starts(b"<?")? {
            self.i += 2;
            self.past(b"?>")?;
        } else if self.starts(b"<!")? {
            // <!DOCTYPE …> with an internal subset in [ ].
            let mut depth = 0;
            loop {
                let Some(c) = self.peek()? else { return Err("a declaration does not end".into()) };
                self.i += 1;
                match c {
                    b'[' => depth += 1,
                    b']' => depth -= 1,
                    b'>' if depth <= 0 => break,
                    _ => {}
                }
            }
        } else {
            return Ok(false);
        }
        Ok(true)
    }

    /// Reads a tag at `<`: (its text without `<` and `>`, self-closing, closing).
    fn tag(&mut self) -> Res<(Vec<u8>, bool, bool)> {
        self.i += 1;
        let mut text = Vec::new();
        let mut quote = 0u8;
        loop {
            let Some(c) = self.peek()? else { return Err("a tag does not end".into()) };
            self.i += 1;
            if quote != 0 {
                if c == quote {
                    quote = 0;
                }
            } else if c == b'"' || c == b'\'' {
                quote = c;
            } else if c == b'>' {
                break;
            }
            if text.len() < 4096 {
                text.push(c);
            }
        }
        let closing = text.first() == Some(&b'/');
        let selfclosing = text.last() == Some(&b'/');
        Ok((text, selfclosing, closing))
    }

    /// Skips the element whose start tag was just read (not self-closing); returns whether it
    /// holds elements.
    fn skip_content(&mut self, name: &[u8]) -> Res<bool> {
        let mut depth = 1usize;
        let mut has = false;
        let mut names: Vec<Vec<u8>> = vec![name.to_vec()];
        while self.seek_lt()? {
            if self.skip_markup()? {
                continue;
            }
            let (text, selfclosing, closing) = self.tag()?;
            if closing {
                let n = tag_name(&text[1..]);
                if names.last().is_some_and(|l| l.as_slice() != n) {
                    return Err(format!("</{}> does not match <{}> at byte {}", String::from_utf8_lossy(n), String::from_utf8_lossy(names.last().map(|v| v.as_slice()).unwrap_or(b"")), self.pos()));
                }
                names.pop();
                depth -= 1;
                if depth == 0 {
                    return Ok(has);
                }
            } else {
                has = true;
                if !selfclosing {
                    depth += 1;
                    names.push(tag_name(&text).to_vec());
                }
            }
        }
        Err(format!("<{}> is never closed", String::from_utf8_lossy(name)))
    }
}

fn tag_name(text: &[u8]) -> &[u8] {
    let end = text.iter().position(|c| c.is_ascii_whitespace() || *c == b'/' || *c == b'>').unwrap_or(text.len());
    &text[..end]
}

fn label(text: &[u8]) -> String {
    let t = String::from_utf8_lossy(text.strip_suffix(b"/").unwrap_or(text));
    let t: String = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() > LABEL_MAX {
        t.chars().take(LABEL_MAX - 1).chain(['…']).collect()
    } else {
        t
    }
}

/// Offset of the root element (after the prolog, comments and the DOCTYPE).
pub fn root(buf: &TextBuf, cancel: &CancelToken) -> Result<u64, String> {
    if buf.codec.unit() != 1 {
        return Err("XML in UTF-16 is not supported here".into());
    }
    let mut s = Src::new(buf, buf.codec.bom_len as u64, cancel);
    while s.seek_lt()? {
        if !s.skip_markup()? {
            return Ok(s.pos());
        }
    }
    Err("no element in the file".into())
}

/// The child elements of the element at `at` (a `<`), up to `max` of them, from `resume`.
pub fn children(buf: &TextBuf, at: u64, resume: Option<Resume>, max: usize, cancel: &CancelToken) -> Result<Page, String> {
    let mut s = Src::new(buf, at, cancel);
    let mut index = 0;
    match resume {
        Some(r) => {
            s = Src::new(buf, r.at, cancel);
            index = r.index;
        }
        None => {
            let (text, selfclosing, _) = s.tag()?;
            if selfclosing {
                return Ok(Page::default());
            }
            let _ = text;
        }
    }
    let mut page = Page::default();
    while s.seek_lt()? {
        let start = s.pos();
        if s.skip_markup()? {
            continue;
        }
        if page.items.len() >= max {
            page.resume = Some(Resume { at: start, index, object: false });
            return Ok(page);
        }
        let (text, selfclosing, closing) = s.tag()?;
        if closing {
            return Ok(page);
        }
        let has = if selfclosing { false } else { s.skip_content(tag_name(&text))? };
        page.items.push(Child { key: Some(label(&text)), index, start, end: s.pos(), kind: if has { Kind::Object } else { Kind::Other } });
        index += 1;
    }
    Ok(page)
}

/// A path step: an element name and its 1-based position among the siblings of that name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XStep {
    pub name: String,
    pub nth: usize,
}

/// Parses `/catalog/item[3]/price` (an index of 1 can be left out).
pub fn parse_path(p: &str) -> Result<Vec<XStep>, String> {
    let p = p.trim();
    let body = p.strip_prefix('/').ok_or("a path starts with /")?;
    body.split('/')
        .filter(|s| !s.is_empty())
        .map(|seg| match seg.split_once('[') {
            Some((name, rest)) => {
                let n: usize = rest.strip_suffix(']').and_then(|n| n.parse().ok()).filter(|&n| n > 0).ok_or(format!("not an index: [{rest}"))?;
                Ok(XStep { name: name.to_string(), nth: n })
            }
            None => Ok(XStep { name: seg.to_string(), nth: 1 }),
        })
        .collect()
}

pub fn format_path(steps: &[XStep]) -> String {
    steps.iter().map(|s| if s.nth > 1 { format!("/{}[{}]", s.name, s.nth) } else { format!("/{}", s.name) }).collect()
}

/// The byte range of the element at `path`.
pub fn resolve(buf: &TextBuf, path: &[XStep], cancel: &CancelToken) -> Result<(u64, u64), String> {
    let at = root(buf, cancel)?;
    let mut s = Src::new(buf, at, cancel);
    let (text, selfclosing, _) = s.tag()?;
    let root_name = String::from_utf8_lossy(tag_name(&text)).into_owned();
    let Some(first) = path.first() else { return Err("the path is empty".into()) };
    if first.name != root_name || first.nth != 1 {
        return Err(format!("the root element is <{root_name}>"));
    }
    let mut range = (at, if selfclosing { s.pos() } else { buf.len() });
    let mut node = at;
    for step in &path[1..] {
        let mut seen = 0;
        let mut found = None;
        let mut resume = None;
        loop {
            let page = children(buf, node, resume, 10_000, cancel)?;
            for c in &page.items {
                let name = c.key.as_deref().map(|k| String::from_utf8_lossy(tag_name(k.as_bytes())).into_owned()).unwrap_or_default();
                if name == step.name {
                    seen += 1;
                    if seen == step.nth {
                        found = Some((c.start, c.end));
                        break;
                    }
                }
            }
            if found.is_some() || page.resume.is_none() {
                break;
            }
            resume = page.resume;
        }
        let Some(r) = found else { return Err(format!("no {} in {}", format_path(std::slice::from_ref(step)), format_path(&path[..1]))) };
        range = r;
        node = r.0;
    }
    Ok(range)
}

/// The path of the innermost element around `off`.
pub fn path_at(buf: &TextBuf, off: u64, cancel: &CancelToken) -> Result<Vec<XStep>, String> {
    let at = root(buf, cancel)?;
    let mut s = Src::new(buf, at, cancel);
    let (text, _, _) = s.tag()?;
    let mut path = vec![XStep { name: String::from_utf8_lossy(tag_name(&text)).into_owned(), nth: 1 }];
    let mut node = at;
    loop {
        let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut next = None;
        let mut resume = None;
        'pages: loop {
            let page = children(buf, node, resume, 10_000, cancel)?;
            for c in &page.items {
                let name = c.key.as_deref().map(|k| String::from_utf8_lossy(tag_name(k.as_bytes())).into_owned()).unwrap_or_default();
                let n = counts.entry(name.clone()).or_insert(0);
                *n += 1;
                if c.start > off {
                    break 'pages;
                }
                if off < c.end {
                    next = Some((c.start, XStep { name, nth: *n }));
                    break 'pages;
                }
            }
            if page.resume.is_none() {
                break;
            }
            resume = page.resume;
        }
        match next {
            Some((start, step)) => {
                path.push(step);
                node = start;
            }
            None => return Ok(path),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::Codec;
    use crate::file::{OpenMode, Source};
    use std::io::Write;
    use std::sync::Arc;

    fn buf(data: &str) -> (tempfile::NamedTempFile, TextBuf) {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(data.as_bytes()).unwrap();
        f.flush().unwrap();
        let src = Arc::new(Source::open(f.path(), OpenMode::Auto).unwrap());
        let idx = crate::index::build_blocking(&src, Codec::utf8());
        let mut t = TextBuf::new(src, idx, Codec::utf8());
        t.blocking = true;
        (f, t)
    }

    const DOC: &str = "<?xml version=\"1.0\"?>\n<!DOCTYPE catalog [ <!ENTITY x \"y\"> ]>\n<!-- a > comment -->\n<catalog>\n  <item id=\"1\"><price>10</price></item>\n  <item id=\"2\" note=\"a > b\"><price>12</price><![CDATA[ <not> ]]></item>\n  <empty/>\n  <item id=\"3\"/>\n</catalog>\n";

    #[test]
    fn children_paths_and_back() {
        let (_f, t) = buf(DOC);
        let c = CancelToken::new();
        let at = root(&t, &c).unwrap();
        assert!(DOC[at as usize..].starts_with("<catalog>"));
        let page = children(&t, at, None, 100, &c).unwrap();
        let labels: Vec<String> = page.items.iter().map(|x| x.key.clone().unwrap()).collect();
        assert_eq!(labels, ["item id=\"1\"", "item id=\"2\" note=\"a > b\"", "empty", "item id=\"3\""]);
        assert_eq!(page.items.iter().map(|x| x.kind).collect::<Vec<_>>(), [Kind::Object, Kind::Object, Kind::Other, Kind::Other]);
        // Pages.
        let first = children(&t, at, None, 2, &c).unwrap();
        assert_eq!(first.items.len(), 2);
        let rest = children(&t, at, first.resume, 2, &c).unwrap();
        assert_eq!(rest.items[0].key.as_deref(), Some("empty"));
        // A path to an element and back.
        let (s, e) = resolve(&t, &parse_path("/catalog/item[2]/price").unwrap(), &c).unwrap();
        assert_eq!(&DOC[s as usize..e as usize], "<price>12</price>");
        assert_eq!(format_path(&path_at(&t, s + 8, &c).unwrap()), "/catalog/item[2]/price");
        assert_eq!(format_path(&path_at(&t, at + 3, &c).unwrap()), "/catalog");
        assert!(resolve(&t, &parse_path("/catalog/item[9]").unwrap(), &c).is_err());
        assert!(resolve(&t, &parse_path("/other").unwrap(), &c).is_err());
        assert!(parse_path("catalog").is_err());
    }

    #[test]
    fn mismatched_and_unclosed() {
        let (_f, t) = buf("<a><b></c></a>");
        assert!(children(&t, 0, None, 10, &CancelToken::new()).unwrap_err().contains("does not match"));
        let (_g, u) = buf("<a><b>");
        assert!(children(&u, 0, None, 10, &CancelToken::new()).is_err());
    }
}
