//! Hiding secrets and personal data in text before it is shared: tokens, cookies, passwords and
//! keys, e-mail addresses and (optionally) IP addresses become labels such as `<email-3f9a2c>`.
//! The same value gets the same label within one run, so lines can still be followed, and the
//! labels come from a hash with a random salt, so they cannot be turned back into the values.
//!
//! The rules find common secrets, not every one: what is shared must still be looked at. Only
//! the value is replaced where there is a name for it (`password=` stays, `Bearer` stays).

use std::io::{self, Write};
use std::ops::Range;

use regex_automata::meta::Regex;
use sha2::{Digest, Sha256};

use crate::encoding::Codec;
use crate::jobs::{CancelToken, Progress};
use crate::text::TextBuf;

/// Which kinds of data are hidden.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// Bearer and Basic credentials, JWTs, Authorization, API key and Cookie headers.
    pub tokens: bool,
    /// Secrets in URLs: `?token=…`, `&api_key=…`, `&sig=…`.
    pub urls: bool,
    /// Named secrets: `password=…`, `"secret": "…"`, `api_key: …`.
    pub keys: bool,
    pub emails: bool,
    /// IPv4 and IPv6 addresses (off by default: logs are often about them).
    pub ips: bool,
    /// More regular expressions; the first group is hidden when there is one, else the match.
    pub custom: Vec<String>,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            tokens: true,
            urls: true,
            keys: true,
            emails: true,
            ips: false,
            custom: Vec::new(),
        }
    }
}

/// The labels' kinds, in the order the counts are reported.
pub const KINDS: [&str; 6] = ["token", "secret", "email", "ip", "custom", "cookie"];

const SECRET_NAMES: &str = "password|passwd|pwd|pass|secret|token|access_token|refresh_token|\
id_token|auth_token|api_?key|apikey|access_?key|secret_?key|private_?key|client_?secret|\
session_?id|sessionid";

/// One rule: the kind of its label and its expression (group 1, when present, is what is hidden).
struct Rule {
    kind: usize,
    re: Regex,
}

fn rule(kind: &str, pattern: &str) -> Result<Rule, String> {
    let re = Regex::new(pattern).map_err(|e| format!("{pattern}: {e}"))?;
    Ok(Rule {
        kind: KINDS.iter().position(|k| *k == kind).unwrap_or(4),
        re,
    })
}

/// Hides what [`Options`] asks for, a line at a time.
pub struct Redactor {
    rules: Vec<Rule>,
    salt: [u8; 16],
    counts: [u64; KINDS.len()],
    spans: Vec<(usize, usize, usize)>,
}

impl Redactor {
    /// `salt` makes the labels; a random one per run keeps them from being reversed.
    pub fn new(opts: &Options, salt: [u8; 16]) -> Result<Redactor, String> {
        let mut rules = Vec::new();
        if opts.tokens {
            rules.push(rule(
                "token",
                r"(?i)\b(?:authorization|proxy-authorization|x-api-key|api-key|x-auth-token|x-access-token)\s*:\s*([^\r\n]+)",
            )?);
            rules.push(rule("cookie", r"(?i)\b(?:set-)?cookie\s*:\s*([^\r\n]+)")?);
            rules.push(rule("token", r"(?i)\bbearer\s+([A-Za-z0-9\-._~+/]+=*)")?);
            rules.push(rule("token", r"(?i)\bbasic\s+([A-Za-z0-9+/]{8,}=*)")?);
            rules.push(rule(
                "token",
                r"\beyJ[A-Za-z0-9_-]{4,}\.[A-Za-z0-9_-]{4,}\.[A-Za-z0-9_-]{4,}",
            )?);
        }
        if opts.urls {
            rules.push(rule(
                "secret",
                &format!(r"(?i)[?&;](?:{SECRET_NAMES}|key|sig|signature|auth|code)=([^&\s#;]+)"),
            )?);
        }
        if opts.keys {
            // "name": "value" (JSON), then name=value / name: value (logfmt, YAML, headers).
            rules.push(rule(
                "secret",
                &format!(r#"(?i)["']?\b(?:{SECRET_NAMES})["']?\s*[:=]\s*"((?:[^"\\]|\\.)*)""#),
            )?);
            rules.push(rule(
                "secret",
                &format!(r#"(?i)\b(?:{SECRET_NAMES})\s*[:=]\s*([^\s,;"'&]+)"#),
            )?);
        }
        if opts.emails {
            rules.push(rule(
                "email",
                r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}\b",
            )?);
        }
        if opts.ips {
            rules.push(rule(
                "ip",
                r"\b(?:(?:25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])\.){3}(?:25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])\b",
            )?);
            rules.push(rule(
                "ip",
                r"(?i)\b(?:[0-9a-f]{1,4}:){7}[0-9a-f]{1,4}\b|\b(?:[0-9a-f]{1,4}:){1,6}:(?:[0-9a-f]{1,4}(?::[0-9a-f]{1,4})*)?\b",
            )?);
        }
        for p in opts.custom.iter().filter(|p| !p.trim().is_empty()) {
            rules.push(rule("custom", p)?);
        }
        Ok(Redactor {
            rules,
            salt,
            counts: [0; KINDS.len()],
            spans: Vec::new(),
        })
    }

    /// How many values of each kind ([`KINDS`]) were hidden so far.
    pub fn counts(&self) -> [u64; KINDS.len()] {
        self.counts
    }

    fn label(&self, kind: usize, value: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(self.salt);
        h.update(value);
        let d = h.finalize();
        format!("<{}-{:02x}{:02x}{:02x}>", KINDS[kind], d[0], d[1], d[2])
    }

    /// Writes `line` (UTF-8 or a single-byte encoding) to `out` with the values hidden; returns
    /// how many were.
    pub fn line(&mut self, line: &[u8], out: &mut Vec<u8>) -> usize {
        self.spans.clear();
        for (i, r) in self.rules.iter().enumerate() {
            for caps in r.re.captures_iter(line) {
                let span = caps
                    .get_group(1)
                    .or_else(|| caps.get_match().map(|m| m.span()));
                if let Some(s) = span.filter(|s| s.end > s.start) {
                    self.spans.push((s.start, s.end, i));
                }
            }
        }
        if self.spans.is_empty() {
            out.extend_from_slice(line);
            return 0;
        }
        // Earliest first; of two starting together, the longer; overlapping ones are dropped.
        self.spans.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
        let mut at = 0;
        let mut n = 0;
        for k in 0..self.spans.len() {
            let (s, e, rule) = self.spans[k];
            if s < at {
                continue;
            }
            let kind = self.rules[rule].kind;
            out.extend_from_slice(&line[at..s]);
            let label = self.label(kind, &line[s..e]);
            out.extend_from_slice(label.as_bytes());
            self.counts[kind] += 1;
            n += 1;
            at = e;
        }
        out.extend_from_slice(&line[at..]);
        n
    }

    /// Like [`Redactor::line`] for text in any encoding: UTF-16 is decoded, hidden and encoded
    /// back.
    pub fn line_in(&mut self, codec: &Codec, line: &[u8], out: &mut Vec<u8>) -> usize {
        if codec.unit() == 1 {
            return self.line(line, out);
        }
        let text = codec.decode_lossy(line);
        let mut tmp = Vec::with_capacity(text.len());
        let n = self.line(text.as_bytes(), &mut tmp);
        out.extend_from_slice(&codec.encode(&String::from_utf8_lossy(&tmp)));
        n
    }
}

/// A random salt for one run's labels.
pub fn random_salt() -> [u8; 16] {
    let mut h = Sha256::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    h.update(now.as_nanos().to_le_bytes());
    h.update(std::process::id().to_le_bytes());
    let local = 0u8;
    h.update((&local as *const u8 as usize).to_le_bytes());
    let d = h.finalize();
    let mut s = [0u8; 16];
    s.copy_from_slice(&d[..16]);
    s
}

/// Longest line handled as one; a longer one is hidden in pieces of this size.
const LINE_MAX: usize = 16 << 20;

/// Writes `range` of the document to `out` with the values hidden, line by line, streaming. Returns
/// the number hidden, or `None` when cancelled.
pub fn redact_to<W: Write + ?Sized>(
    buf: &TextBuf,
    range: Range<u64>,
    r: &mut Redactor,
    out: &mut W,
    cancel: &CancelToken,
    progress: Option<&Progress>,
) -> io::Result<Option<u64>> {
    let codec = buf.codec;
    let unit = codec.unit();
    let mut line: Vec<u8> = Vec::new();
    let mut done: Vec<u8> = Vec::new();
    let mut hidden = 0u64;
    let mut cancelled = false;
    let mut err = None;
    if let Some(p) = progress {
        p.set_total(range.end.saturating_sub(range.start));
    }
    let mut flush = |line: &mut Vec<u8>, done: &mut Vec<u8>, hidden: &mut u64| -> io::Result<()> {
        done.clear();
        *hidden += r.line_in(&codec, line, done) as u64;
        line.clear();
        out.write_all(done)
    };
    let read = buf.for_each_chunk(range.clone(), |base, chunk| {
        if cancel.is_cancelled() {
            cancelled = true;
            return false;
        }
        let mut from = 0;
        while from < chunk.len() {
            let end = codec
                .find_nl(&chunk[from..], base + from as u64)
                .map(|i| from + i + unit);
            let stop = end.unwrap_or(chunk.len());
            line.extend_from_slice(&chunk[from..stop]);
            from = stop;
            if end.is_some() || line.len() >= LINE_MAX {
                if let Err(e) = flush(&mut line, &mut done, &mut hidden) {
                    err = Some(e);
                    return false;
                }
            }
        }
        if let Some(p) = progress {
            p.set_done(base + chunk.len() as u64 - range.start);
        }
        true
    });
    if let Some(e) = err {
        return Err(e);
    }
    if cancelled {
        return Ok(None);
    }
    if read.is_none() {
        return Err(io::Error::other("could not read the document"));
    }
    if !line.is_empty() {
        flush(&mut line, &mut done, &mut hidden)?;
    }
    Ok(Some(hidden))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::{Encoding, NlByte};
    use crate::file::{OpenMode, Source};
    use crate::index;
    use std::sync::Arc;

    fn hide(opts: &Options, text: &str) -> String {
        let mut r = Redactor::new(opts, [7; 16]).unwrap();
        let mut out = Vec::new();
        r.line(text.as_bytes(), &mut out);
        String::from_utf8(out).unwrap()
    }

    fn masked(s: &str) -> String {
        // Labels with their hashes replaced, so the expected text can be written out.
        let re = Regex::new(r"<([a-z]+)-[0-9a-f]{6}>").unwrap();
        let mut out = String::new();
        let mut at = 0;
        for m in re.find_iter(s.as_bytes()) {
            out.push_str(&s[at..m.start()]);
            let kind = &s[m.start() + 1..m.end() - 8];
            out.push_str(&format!("<{kind}>"));
            at = m.end();
        }
        out.push_str(&s[at..]);
        out
    }

    #[test]
    fn hides_tokens_secrets_and_emails_but_keeps_names() {
        let o = Options::default();
        let cases = [
            (
                "Authorization: Bearer abc.def-123",
                "Authorization: <token>",
            ),
            (
                "curl -H 'x: y' bearer eyJxx.yy.zz",
                "curl -H 'x: y' bearer <token>",
            ),
            ("Cookie: sid=1; theme=dark", "Cookie: <cookie>"),
            (
                "GET /api?user=7&token=s3cr3t&x=1",
                "GET /api?user=7&token=<secret>&x=1",
            ),
            (
                r#"{"user":"ann","password":"hunter 2"}"#,
                r#"{"user":"ann","password":"<secret>"}"#,
            ),
            (
                "level=info api_key=AKIA123 msg=ok",
                "level=info api_key=<secret> msg=ok",
            ),
            ("mail ann.lee+x@example.co.uk now", "mail <email> now"),
            ("token eyJhbGciOi.eyJzdWIiOi.SflKxwRJSM", "token <token>"),
            (
                "plain line, nothing here 10.0.0.1",
                "plain line, nothing here 10.0.0.1",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(masked(&hide(&o, input)), want, "{input}");
        }
        let ips = Options {
            ips: true,
            ..Options::default()
        };
        assert_eq!(
            masked(&hide(&ips, "from 10.0.0.1 and fe80::1:2")),
            "from <ip> and <ip>"
        );
        let custom = Options {
            custom: vec![r"order-(\d+)".into()],
            ..Options::default()
        };
        assert_eq!(
            masked(&hide(&custom, "paid order-981 ok")),
            "paid order-<custom> ok"
        );
        assert!(Redactor::new(
            &Options {
                custom: vec!["(".into()],
                ..Options::default()
            },
            [0; 16]
        )
        .is_err());
    }

    #[test]
    fn the_same_value_gets_the_same_label_and_counts_add_up() {
        let mut r = Redactor::new(&Options::default(), [1; 16]).unwrap();
        let mut a = Vec::new();
        r.line(b"ann@example.com bob@example.com ann@example.com", &mut a);
        let s = String::from_utf8(a).unwrap();
        let labels: Vec<&str> = s.split(' ').collect();
        assert_eq!(labels[0], labels[2]);
        assert_ne!(labels[0], labels[1]);
        assert_eq!(r.counts()[2], 3);
        // Another salt, other labels.
        let mut r2 = Redactor::new(&Options::default(), [2; 16]).unwrap();
        let mut b = Vec::new();
        r2.line(b"ann@example.com", &mut b);
        assert_ne!(String::from_utf8(b).unwrap(), labels[0]);
    }

    fn buf_with(data: &[u8], codec: Codec) -> (tempfile::NamedTempFile, TextBuf) {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(data).unwrap();
        f.flush().unwrap();
        let src = Arc::new(Source::open(f.path(), OpenMode::Auto).unwrap());
        let idx = index::build_blocking(&src, codec);
        let mut t = TextBuf::new(src, idx, codec);
        t.blocking = true;
        (f, t)
    }

    #[test]
    fn streams_a_document_across_chunks_and_in_utf16() {
        let mut text = String::new();
        for i in 0..400_000 {
            text.push_str(&format!("{i} user{i}@example.com token=t{i} ok\n"));
        }
        let (_f, b) = buf_with(text.as_bytes(), Codec::utf8());
        let mut r = Redactor::new(&Options::default(), [3; 16]).unwrap();
        let mut out = Vec::new();
        let n = redact_to(&b, 0..b.len(), &mut r, &mut out, &CancelToken::new(), None)
            .unwrap()
            .unwrap();
        assert_eq!(n, 800_000);
        let out = String::from_utf8(out).unwrap();
        assert_eq!(out.lines().count(), 400_000);
        assert!(!out.contains("@example.com") && !out.contains("token=t"));
        let wide: Vec<u8> = "a token=x1 b\nmail z@q.io\n"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        let codec = Codec::new(Encoding::Utf16Le, NlByte::Lf, 0);
        let (_f, b) = buf_with(&wide, codec);
        let mut out = Vec::new();
        redact_to(&b, 0..b.len(), &mut r, &mut out, &CancelToken::new(), None)
            .unwrap()
            .unwrap();
        let back = codec.decode_lossy(&out);
        assert_eq!(masked(&back), "a token=<secret> b\nmail <email>\n");
    }
}
