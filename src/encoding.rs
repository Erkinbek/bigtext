//! Encodings, line-break handling and detection.
//!
//! Text is always kept as raw bytes in the file's encoding. A [`Codec`] knows how to find line
//! breaks in those bytes, decode/encode characters and snap offsets to character boundaries.
//! Decoding never fails: invalid sequences become U+FFFD with a well-defined byte length.

use std::collections::HashMap;
use std::sync::OnceLock;

pub const REPLACEMENT: char = '\u{FFFD}';

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    Windows1252,
    Windows1251,
    Latin1,
}

impl Encoding {
    pub const ALL: [Encoding; 6] = [
        Encoding::Utf8,
        Encoding::Utf16Le,
        Encoding::Utf16Be,
        Encoding::Windows1252,
        Encoding::Windows1251,
        Encoding::Latin1,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Encoding::Utf8 => "UTF-8",
            Encoding::Utf16Le => "UTF-16 LE",
            Encoding::Utf16Be => "UTF-16 BE",
            Encoding::Windows1252 => "Windows-1252",
            Encoding::Windows1251 => "Windows-1251",
            Encoding::Latin1 => "ISO-8859-1",
        }
    }

    pub fn unit(self) -> usize {
        match self {
            Encoding::Utf16Le | Encoding::Utf16Be => 2,
            _ => 1,
        }
    }

    pub fn is_utf16(self) -> bool {
        self.unit() == 2
    }

    pub fn is_single_byte(self) -> bool {
        matches!(self, Encoding::Windows1252 | Encoding::Windows1251 | Encoding::Latin1)
    }

    pub fn bom(self) -> &'static [u8] {
        match self {
            Encoding::Utf8 => &[0xEF, 0xBB, 0xBF],
            Encoding::Utf16Le => &[0xFF, 0xFE],
            Encoding::Utf16Be => &[0xFE, 0xFF],
            _ => &[],
        }
    }
}

/// Which character splits lines. CRLF files use `Lf` (the CR is hidden in the view).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NlByte {
    Lf,
    Cr,
}

/// Detected line-ending style (for display and conversion).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineEnding {
    Lf,
    CrLf,
    Cr,
    Mixed,
    Unknown,
}

impl LineEnding {
    pub fn name(self) -> &'static str {
        match self {
            LineEnding::Lf => "LF",
            LineEnding::CrLf => "CRLF",
            LineEnding::Cr => "CR",
            LineEnding::Mixed => "Mixed",
            LineEnding::Unknown => "—",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Codec {
    pub enc: Encoding,
    pub nl: NlByte,
    pub bom_len: usize,
}

fn single_byte_table(enc: Encoding) -> &'static [char; 256] {
    static T1252: OnceLock<[char; 256]> = OnceLock::new();
    static T1251: OnceLock<[char; 256]> = OnceLock::new();
    static TLATIN1: OnceLock<[char; 256]> = OnceLock::new();
    let build = |e: &'static encoding_rs::Encoding| {
        let mut t = [REPLACEMENT; 256];
        for (b, slot) in t.iter_mut().enumerate() {
            if b < 0x80 {
                *slot = b as u8 as char;
            } else {
                let byte = [b as u8];
                let (s, _) = e.decode_without_bom_handling(&byte);
                *slot = s.chars().next().unwrap_or(REPLACEMENT);
            }
        }
        t
    };
    match enc {
        Encoding::Windows1252 => T1252.get_or_init(|| build(encoding_rs::WINDOWS_1252)),
        Encoding::Windows1251 => T1251.get_or_init(|| build(encoding_rs::WINDOWS_1251)),
        _ => TLATIN1.get_or_init(|| {
            let mut t = [REPLACEMENT; 256];
            for (b, slot) in t.iter_mut().enumerate() {
                *slot = char::from(b as u8);
            }
            t
        }),
    }
}

fn single_byte_reverse(enc: Encoding) -> &'static HashMap<char, u8> {
    static R1252: OnceLock<HashMap<char, u8>> = OnceLock::new();
    static R1251: OnceLock<HashMap<char, u8>> = OnceLock::new();
    static RLATIN1: OnceLock<HashMap<char, u8>> = OnceLock::new();
    let build = || {
        let t = single_byte_table(enc);
        let mut m = HashMap::new();
        for (b, &c) in t.iter().enumerate() {
            if c != REPLACEMENT {
                m.entry(c).or_insert(b as u8);
            }
        }
        m
    };
    match enc {
        Encoding::Windows1252 => R1252.get_or_init(build),
        Encoding::Windows1251 => R1251.get_or_init(build),
        _ => RLATIN1.get_or_init(build),
    }
}

/// Counts occurrences of `needle` in `hay`; written so LLVM vectorises it (SSE2 baseline).
pub fn count_byte(hay: &[u8], needle: u8) -> u64 {
    const LANES: usize = 32;
    let mut total = 0u64;
    let mut blocks = hay.chunks_exact(LANES * 255);
    for block in &mut blocks {
        let mut acc = [0u8; LANES];
        for c in block.chunks_exact(LANES) {
            for i in 0..LANES {
                acc[i] += (c[i] == needle) as u8;
            }
        }
        total += acc.iter().map(|&x| x as u64).sum::<u64>();
    }
    let rem = blocks.remainder();
    let mut lanes = rem.chunks_exact(LANES);
    let mut acc = [0u8; LANES];
    for c in &mut lanes {
        for i in 0..LANES {
            acc[i] += (c[i] == needle) as u8;
        }
    }
    total += acc.iter().map(|&x| x as u64).sum::<u64>();
    total + lanes.remainder().iter().filter(|&&b| b == needle).count() as u64
}

/// Decodes one UTF-8 character; invalid bytes decode as U+FFFD of length 1.
#[inline]
pub fn utf8_char(b: &[u8]) -> (char, usize) {
    let b0 = b[0];
    if b0 < 0x80 {
        return (b0 as char, 1);
    }
    let need = match b0 {
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => return (REPLACEMENT, 1),
    };
    if b.len() < need {
        return (REPLACEMENT, 1);
    }
    match std::str::from_utf8(&b[..need]) {
        Ok(s) => (s.chars().next().unwrap_or(REPLACEMENT), need),
        Err(_) => (REPLACEMENT, 1),
    }
}

impl Codec {
    pub fn new(enc: Encoding, nl: NlByte, bom_len: usize) -> Codec {
        Codec { enc, nl, bom_len }
    }

    pub fn utf8() -> Codec {
        Codec { enc: Encoding::Utf8, nl: NlByte::Lf, bom_len: 0 }
    }

    #[inline]
    pub fn unit(&self) -> usize {
        self.enc.unit()
    }

    /// Bytes of one newline unit.
    pub fn nl_unit(&self) -> &'static [u8] {
        match (self.enc, self.nl) {
            (Encoding::Utf16Le, NlByte::Lf) => b"\n\0",
            (Encoding::Utf16Le, NlByte::Cr) => b"\r\0",
            (Encoding::Utf16Be, NlByte::Lf) => b"\0\n",
            (Encoding::Utf16Be, NlByte::Cr) => b"\0\r",
            (_, NlByte::Lf) => b"\n",
            (_, NlByte::Cr) => b"\r",
        }
    }

    /// The bytes of a CR unit (used to hide the CR of CRLF in LF mode).
    pub fn cr_unit(&self) -> &'static [u8] {
        match self.enc {
            Encoding::Utf16Le => b"\r\0",
            Encoding::Utf16Be => b"\0\r",
            _ => b"\r",
        }
    }

    #[inline]
    fn nl_char_byte(&self) -> u8 {
        match self.nl {
            NlByte::Lf => b'\n',
            NlByte::Cr => b'\r',
        }
    }

    /// Index of the first newline unit in `hay`, whose first byte is at absolute offset `base`.
    pub fn find_nl(&self, hay: &[u8], base: u64) -> Option<usize> {
        let c = self.nl_char_byte();
        match self.enc {
            Encoding::Utf16Le => {
                let mut from = 0;
                while let Some(i) = memchr::memchr(c, &hay[from..]) {
                    let p = from + i;
                    if (base + p as u64) % 2 == 0 && p + 1 < hay.len() && hay[p + 1] == 0 {
                        return Some(p);
                    }
                    from = p + 1;
                }
                None
            }
            Encoding::Utf16Be => {
                let mut from = 0;
                while let Some(i) = memchr::memchr(c, &hay[from..]) {
                    let p = from + i;
                    if p >= 1 && (base + p as u64) % 2 == 1 && hay[p - 1] == 0 {
                        return Some(p - 1);
                    }
                    from = p + 1;
                }
                None
            }
            _ => memchr::memchr(c, hay),
        }
    }

    /// Index of the last newline unit fully contained in `hay`.
    pub fn rfind_nl(&self, hay: &[u8], base: u64) -> Option<usize> {
        let c = self.nl_char_byte();
        match self.enc {
            Encoding::Utf16Le => {
                let mut to = hay.len();
                while let Some(p) = memchr::memrchr(c, &hay[..to]) {
                    if (base + p as u64) % 2 == 0 && p + 1 < hay.len() && hay[p + 1] == 0 {
                        return Some(p);
                    }
                    to = p;
                }
                None
            }
            Encoding::Utf16Be => {
                let mut to = hay.len();
                while let Some(p) = memchr::memrchr(c, &hay[..to]) {
                    if p >= 1 && (base + p as u64) % 2 == 1 && hay[p - 1] == 0 {
                        return Some(p - 1);
                    }
                    to = p;
                }
                None
            }
            _ => memchr::memrchr(c, hay),
        }
    }

    /// Number of newline units in `hay`.
    pub fn count_nl(&self, hay: &[u8], base: u64) -> u64 {
        if self.enc.is_utf16() {
            let start = (base % 2) as usize;
            if start >= hay.len() {
                return 0;
            }
            let u = self.nl_unit();
            let (a, b) = (u[0], u[1]);
            hay[start..].chunks_exact(2).filter(|p| p[0] == a && p[1] == b).count() as u64
        } else {
            count_byte(hay, self.nl_char_byte())
        }
    }

    /// Position (index into `hay`) of the `n`-th (1-based) newline unit.
    pub fn nth_nl(&self, hay: &[u8], base: u64, n: u64) -> Option<usize> {
        let mut seen = 0;
        let mut from = 0;
        while from < hay.len() {
            let i = self.find_nl(&hay[from..], base + from as u64)?;
            seen += 1;
            if seen == n {
                return Some(from + i);
            }
            from += i + self.unit();
        }
        None
    }

    /// True if `b` starts with a newline unit.
    #[inline]
    pub fn is_nl_at(&self, b: &[u8]) -> bool {
        b.starts_with(self.nl_unit())
    }

    /// Decodes one character at the start of `b` (non-empty). Returns (char, byte length).
    #[inline]
    pub fn decode_char(&self, b: &[u8]) -> (char, usize) {
        match self.enc {
            Encoding::Utf8 => utf8_char(b),
            Encoding::Utf16Le | Encoding::Utf16Be => {
                if b.len() < 2 {
                    return (REPLACEMENT, b.len().max(1));
                }
                let rd = |i: usize| -> u16 {
                    if self.enc == Encoding::Utf16Le {
                        u16::from_le_bytes([b[i], b[i + 1]])
                    } else {
                        u16::from_be_bytes([b[i], b[i + 1]])
                    }
                };
                let u = rd(0);
                match u {
                    0xD800..=0xDBFF => {
                        if b.len() >= 4 {
                            let l = rd(2);
                            if (0xDC00..=0xDFFF).contains(&l) {
                                let c = 0x10000 + (((u as u32) - 0xD800) << 10) + ((l as u32) - 0xDC00);
                                return (char::from_u32(c).unwrap_or(REPLACEMENT), 4);
                            }
                        }
                        (REPLACEMENT, 2)
                    }
                    0xDC00..=0xDFFF => (REPLACEMENT, 2),
                    _ => (char::from_u32(u as u32).unwrap_or(REPLACEMENT), 2),
                }
            }
            _ => (single_byte_table(self.enc)[b[0] as usize], 1),
        }
    }

    /// Length of the character that ends exactly at the end of `before` (≥1 if non-empty).
    pub fn prev_char_len(&self, before: &[u8]) -> usize {
        let n = before.len();
        if n == 0 {
            return 0;
        }
        match self.enc {
            Encoding::Utf8 => {
                for k in 1..=n.min(4) {
                    let s = n - k;
                    if before[s] & 0xC0 != 0x80 {
                        let (_, l) = utf8_char(&before[s..]);
                        return if l == k { k } else { 1 };
                    }
                }
                1
            }
            Encoding::Utf16Le | Encoding::Utf16Be => {
                if n < 2 {
                    return n;
                }
                let rd = |i: usize| -> u16 {
                    if self.enc == Encoding::Utf16Le {
                        u16::from_le_bytes([before[i], before[i + 1]])
                    } else {
                        u16::from_be_bytes([before[i], before[i + 1]])
                    }
                };
                let last = rd(n - 2);
                if (0xDC00..=0xDFFF).contains(&last) && n >= 4 && (0xD800..=0xDBFF).contains(&rd(n - 4)) {
                    4
                } else {
                    2
                }
            }
            _ => 1,
        }
    }

    /// Number of bytes to skip at the start of `b` to land on a character boundary.
    pub fn snap_forward(&self, b: &[u8]) -> usize {
        match self.enc {
            Encoding::Utf8 => b.iter().take(3).take_while(|&&x| x & 0xC0 == 0x80).count(),
            Encoding::Utf16Le | Encoding::Utf16Be => {
                if b.len() >= 2 {
                    let u = if self.enc == Encoding::Utf16Le {
                        u16::from_le_bytes([b[0], b[1]])
                    } else {
                        u16::from_be_bytes([b[0], b[1]])
                    };
                    if (0xDC00..=0xDFFF).contains(&u) {
                        return 2;
                    }
                }
                0
            }
            _ => 0,
        }
    }

    /// Encodes text into this encoding (unmappable characters become '?').
    pub fn encode(&self, s: &str) -> Vec<u8> {
        match self.enc {
            Encoding::Utf8 => s.as_bytes().to_vec(),
            Encoding::Utf16Le => s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect(),
            Encoding::Utf16Be => s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect(),
            _ => {
                let rev = single_byte_reverse(self.enc);
                s.chars().map(|c| if (c as u32) < 0x80 { c as u8 } else { *rev.get(&c).unwrap_or(&b'?') }).collect()
            }
        }
    }

    /// True if every character of `s` can be represented in this encoding.
    pub fn can_encode(&self, s: &str) -> bool {
        match self.enc {
            Encoding::Utf8 | Encoding::Utf16Le | Encoding::Utf16Be => true,
            _ => {
                let rev = single_byte_reverse(self.enc);
                s.chars().all(|c| (c as u32) < 0x80 || rev.contains_key(&c))
            }
        }
    }

    /// Lossy decode to a String.
    pub fn decode_lossy(&self, b: &[u8]) -> String {
        if self.enc == Encoding::Utf8 {
            return String::from_utf8_lossy(b).into_owned();
        }
        let mut out = String::with_capacity(b.len());
        self.decode_into(b, &mut out);
        out
    }

    /// Appends the decoded characters of `b` to `out`.
    pub fn decode_into(&self, b: &[u8], out: &mut String) {
        match self.enc {
            Encoding::Utf8 => out.push_str(&String::from_utf8_lossy(b)),
            e if e.is_single_byte() => {
                let t = single_byte_table(e);
                out.extend(b.iter().map(|&x| t[x as usize]));
            }
            _ => {
                let mut i = 0;
                while i < b.len() {
                    let (c, l) = self.decode_char(&b[i..]);
                    out.push(c);
                    i += l;
                }
            }
        }
    }

    /// Source byte width of a decoded character (exact inverse of `decode_char` for non-UTF-8).
    #[inline]
    pub fn src_width(&self, c: char) -> usize {
        match self.enc {
            Encoding::Utf8 => c.len_utf8(),
            Encoding::Utf16Le | Encoding::Utf16Be => c.len_utf16() * 2,
            _ => 1,
        }
    }

    /// Bytes to insert for a line break in the given style.
    pub fn line_break(&self, style: LineEnding) -> Vec<u8> {
        let s = match (style, self.nl) {
            (_, NlByte::Cr) | (LineEnding::Cr, _) => "\r",
            (LineEnding::CrLf, _) => "\r\n",
            (LineEnding::Lf, _) | (LineEnding::Mixed, _) => "\n",
            (LineEnding::Unknown, _) => {
                if cfg!(windows) {
                    "\r\n"
                } else {
                    "\n"
                }
            }
        };
        self.encode(s)
    }
}

/// Result of sniffing the beginning of a file.
#[derive(Clone, Copy, Debug)]
pub struct Detected {
    pub codec: Codec,
    pub line_ending: LineEnding,
    pub looks_binary: bool,
}

/// Detects encoding, line endings and binary-ness from a sample (normally the first 1 MB).
pub fn detect(sample: &[u8], whole_file: bool) -> Detected {
    let (enc, bom_len) = detect_encoding(sample, whole_file);
    detect_with(sample, whole_file, enc, bom_len)
}

/// Like [`detect`] but with a forced encoding.
pub fn detect_with(sample: &[u8], whole_file: bool, enc: Encoding, bom_len: usize) -> Detected {
    let bom_len = if sample.starts_with(enc.bom()) && !enc.bom().is_empty() { enc.bom().len() } else { bom_len.min(sample.len()) };
    let content = &sample[bom_len.min(sample.len())..];
    let (nl, line_ending) = detect_line_endings(content, enc, whole_file);
    let looks_binary = looks_binary(content, enc);
    Detected { codec: Codec::new(enc, nl, bom_len), line_ending, looks_binary }
}

pub fn detect_encoding(s: &[u8], whole_file: bool) -> (Encoding, usize) {
    if s.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return (Encoding::Utf8, 3);
    }
    if s.starts_with(&[0xFF, 0xFE]) {
        return (Encoding::Utf16Le, 2);
    }
    if s.starts_with(&[0xFE, 0xFF]) {
        return (Encoding::Utf16Be, 2);
    }
    let head = &s[..s.len().min(64 * 1024)];
    if head.len() >= 4 {
        let pairs = head.len() / 2;
        let (mut ze, mut zo) = (0usize, 0usize);
        for p in head.chunks_exact(2) {
            ze += (p[0] == 0) as usize;
            zo += (p[1] == 0) as usize;
        }
        if zo * 10 > pairs * 3 && ze * 20 < pairs {
            return (Encoding::Utf16Le, 0);
        }
        if ze * 10 > pairs * 3 && zo * 20 < pairs {
            return (Encoding::Utf16Be, 0);
        }
    }
    match std::str::from_utf8(s) {
        Ok(_) => return (Encoding::Utf8, 0),
        Err(e) if e.error_len().is_none() && !whole_file => return (Encoding::Utf8, 0),
        Err(_) => {}
    }
    // Mostly-UTF-8 with a few bad bytes still counts as UTF-8.
    let (mut multibyte, mut bad) = (0usize, 0usize);
    let mut i = 0;
    while i < s.len() {
        if s[i] < 0x80 {
            i += 1;
            continue;
        }
        let (c, l) = utf8_char(&s[i..]);
        if c == REPLACEMENT && l == 1 {
            bad += 1;
        } else {
            multibyte += 1;
        }
        i += l;
    }
    if multibyte > 0 && bad * 20 < multibyte {
        return (Encoding::Utf8, 0);
    }
    (single_byte_guess(s), 0)
}

fn single_byte_guess(s: &[u8]) -> Encoding {
    let (mut hi, mut mid, mut letters, mut undefined_1252) = (0usize, 0usize, 0usize, 0usize);
    for &b in s {
        match b {
            0xC0..=0xFF => hi += 1,
            0x81 | 0x8D | 0x8F | 0x90 | 0x9D => {
                undefined_1252 += 1;
                mid += 1
            }
            0x80..=0xBF => mid += 1,
            b if b.is_ascii_alphabetic() => letters += 1,
            _ => {}
        }
    }
    // Cyrillic text in 1251 is dominated by bytes 0xC0..0xFF; Western text has few of them.
    if hi > mid && hi * 10 > (hi + letters) * 3 {
        Encoding::Windows1251
    } else if undefined_1252 > 0 {
        Encoding::Latin1
    } else {
        Encoding::Windows1252
    }
}

fn looks_binary(content: &[u8], enc: Encoding) -> bool {
    let head = &content[..content.len().min(64 * 1024)];
    if head.is_empty() {
        return false;
    }
    if enc.is_utf16() {
        let units = head.len() / 2;
        let nul_units = head.chunks_exact(2).filter(|p| p[0] == 0 && p[1] == 0).count();
        return units > 0 && nul_units * 100 > units;
    }
    let nul = count_byte(head, 0) as usize;
    let ctrl = head.iter().filter(|&&b| b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r' | 0x0C | 0x1B)).count();
    nul * 100 > head.len() || ctrl * 10 > head.len()
}

fn detect_line_endings(content: &[u8], enc: Encoding, whole_file: bool) -> (NlByte, LineEnding) {
    let unit = enc.unit();
    let unit_at = |i: usize| -> u16 {
        match enc {
            Encoding::Utf16Le => u16::from_le_bytes([content[i], content[i + 1]]),
            Encoding::Utf16Be => u16::from_be_bytes([content[i], content[i + 1]]),
            _ => content[i] as u16,
        }
    };
    let n = content.len() / unit * unit;
    let (mut crlf, mut lf, mut cr) = (0u64, 0u64, 0u64);
    let mut i = 0;
    while i < n {
        let u = unit_at(i);
        if u == 0x0A {
            lf += 1;
        } else if u == 0x0D {
            if i + unit < n {
                if unit_at(i + unit) == 0x0A {
                    crlf += 1;
                    i += unit;
                } else {
                    cr += 1;
                }
            } else if whole_file {
                cr += 1;
            }
        }
        i += unit;
    }
    if lf + crlf > 0 {
        let style = if cr > 0 || (crlf > 0 && lf > 0) {
            LineEnding::Mixed
        } else if crlf > 0 {
            LineEnding::CrLf
        } else {
            LineEnding::Lf
        };
        (NlByte::Lf, style)
    } else if cr > 0 {
        (NlByte::Cr, LineEnding::Cr)
    } else {
        (NlByte::Lf, LineEnding::Unknown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_byte_matches_naive() {
        let data: Vec<u8> = (0..100_003u32).map(|i| if i % 7 == 0 { b'\n' } else { (i % 200) as u8 + 20 }).collect();
        let naive = data.iter().filter(|&&b| b == b'\n').count() as u64;
        assert_eq!(count_byte(&data, b'\n'), naive);
        assert_eq!(count_byte(&data[..13], b'\n'), data[..13].iter().filter(|&&b| b == b'\n').count() as u64);
    }

    #[test]
    fn detects_boms() {
        assert_eq!(detect_encoding(b"\xEF\xBB\xBFhi", true), (Encoding::Utf8, 3));
        assert_eq!(detect_encoding(b"\xFF\xFEh\0i\0", true), (Encoding::Utf16Le, 2));
        assert_eq!(detect_encoding(b"\xFE\xFF\0h\0i", true), (Encoding::Utf16Be, 2));
    }

    #[test]
    fn detects_utf16_without_bom() {
        let le: Vec<u8> = "hello world\nsecond line\n".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(detect_encoding(&le, true).0, Encoding::Utf16Le);
        let be: Vec<u8> = "hello world\nsecond line\n".encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
        assert_eq!(detect_encoding(&be, true).0, Encoding::Utf16Be);
    }

    #[test]
    fn detects_utf8_and_single_byte() {
        assert_eq!(detect_encoding("привет мир\n".as_bytes(), true).0, Encoding::Utf8);
        let (cp1251, _, _) = encoding_rs::WINDOWS_1251.encode("Съешь же ещё этих мягких французских булок\n");
        assert_eq!(detect_encoding(&cp1251, true).0, Encoding::Windows1251);
        let (cp1252, _, _) = encoding_rs::WINDOWS_1252.encode("Le café est très bon à Paris, déjà vu.\n");
        assert_eq!(detect_encoding(&cp1252, true).0, Encoding::Windows1252);
        // Truncated multibyte char at the end of a partial sample is still UTF-8.
        let s = "абв".as_bytes();
        assert_eq!(detect_encoding(&s[..s.len() - 1], false).0, Encoding::Utf8);
    }

    #[test]
    fn detects_line_endings() {
        assert_eq!(detect(b"a\r\nb\r\n", true).line_ending, LineEnding::CrLf);
        assert_eq!(detect(b"a\nb\n", true).line_ending, LineEnding::Lf);
        assert_eq!(detect(b"a\r\nb\n", true).line_ending, LineEnding::Mixed);
        let d = detect(b"a\rb\rc", true);
        assert_eq!((d.line_ending, d.codec.nl), (LineEnding::Cr, NlByte::Cr));
        assert_eq!(detect(b"abc", true).line_ending, LineEnding::Unknown);
    }

    #[test]
    fn detects_binary() {
        let mut v = vec![0u8; 1000];
        v.extend_from_slice(b"some text");
        assert!(detect(&v, true).looks_binary);
        assert!(!detect(b"plain text\n", true).looks_binary);
    }

    #[test]
    fn utf16_newline_alignment() {
        let c = Codec::new(Encoding::Utf16Le, NlByte::Lf, 0);
        // U+0A0A ("\n\n" bytes) is not a newline; a real newline follows.
        let bytes = [0x0A, 0x0A, 0x41, 0x00, 0x0A, 0x00];
        assert_eq!(c.find_nl(&bytes, 0), Some(4));
        assert_eq!(c.count_nl(&bytes, 0), 1);
        assert_eq!(c.rfind_nl(&bytes, 0), Some(4));
        let be = Codec::new(Encoding::Utf16Be, NlByte::Lf, 0);
        let bytes = [0x0A, 0x0A, 0x00, 0x41, 0x00, 0x0A];
        assert_eq!(be.find_nl(&bytes, 0), Some(4));
        assert_eq!(be.count_nl(&bytes, 0), 1);
    }

    #[test]
    fn decode_invalid_never_panics() {
        let c = Codec::utf8();
        let bytes = [0xFF, 0xC3, b'a', 0xE2, 0x82];
        let mut i = 0;
        let mut out = Vec::new();
        while i < bytes.len() {
            let (ch, l) = c.decode_char(&bytes[i..]);
            out.push(ch);
            i += l;
        }
        assert_eq!(out, vec![REPLACEMENT, REPLACEMENT, 'a', REPLACEMENT, REPLACEMENT]);
        let u16c = Codec::new(Encoding::Utf16Le, NlByte::Lf, 0);
        assert_eq!(u16c.decode_char(&[0x00, 0xD8, 0x41, 0x00]), (REPLACEMENT, 2));
        assert_eq!(u16c.decode_char(&[0x3D, 0xD8, 0x00, 0xDE]), ('😀', 4));
        assert_eq!(u16c.decode_char(&[0x41]), (REPLACEMENT, 1));
    }

    #[test]
    fn prev_char_and_snap() {
        let c = Codec::utf8();
        let s = "aé😀".as_bytes();
        assert_eq!(c.prev_char_len(s), 4);
        assert_eq!(c.prev_char_len(&s[..3]), 2);
        assert_eq!(c.snap_forward(&s[4..]), 3);
        assert_eq!(c.snap_forward(&s[3..]), 0);
    }

    #[test]
    fn single_byte_roundtrip() {
        let c = Codec::new(Encoding::Windows1251, NlByte::Lf, 0);
        let bytes = c.encode("Привет, мир!");
        assert_eq!(c.decode_lossy(&bytes), "Привет, мир!");
        assert!(!c.can_encode("日本"));
    }
}
