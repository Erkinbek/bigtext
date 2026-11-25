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
        matches!(
            self,
            Encoding::Windows1252 | Encoding::Windows1251 | Encoding::Latin1
        )
    }

    /// UTF-8 or UTF-16: every character, U+FFFD included, can be written.
    pub fn is_unicode(self) -> bool {
        !self.is_single_byte()
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

/// Counts occurrences of `needle` in `hay`.
///
/// The hot loop of indexing and of every whole-file line count. LLVM does not vectorise the
/// portable form well (about 2.3 GB/s), so x86_64 (SSE2, always present) and aarch64 (NEON)
/// use explicit SIMD: compare 16 bytes, subtract the 0xFF mask from byte counters, and sum the
/// counters every 255 rounds before they can overflow (about 12 GB/s).
pub fn count_byte(hay: &[u8], needle: u8) -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        simd::count_byte_sse2(hay, needle)
    }
    #[cfg(target_arch = "aarch64")]
    {
        simd::count_byte_neon(hay, needle)
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        count_byte_portable(hay, needle)
    }
}

/// Counts the 2-byte units equal to `unit` (as laid out in memory) in `hay`, taken in pairs
/// from its start; a trailing odd byte is ignored. Used for UTF-16 newlines.
pub fn count_u16(hay: &[u8], unit: [u8; 2]) -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        simd::count_u16_sse2(hay, unit)
    }
    #[cfg(target_arch = "aarch64")]
    {
        simd::count_u16_neon(hay, unit)
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        count_u16_portable(hay, unit)
    }
}

#[cfg_attr(any(target_arch = "x86_64", target_arch = "aarch64"), allow(dead_code))]
fn count_byte_portable(hay: &[u8], needle: u8) -> u64 {
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
    total + blocks.remainder().iter().filter(|&&b| b == needle).count() as u64
}

#[cfg_attr(any(target_arch = "x86_64", target_arch = "aarch64"), allow(dead_code))]
fn count_u16_portable(hay: &[u8], unit: [u8; 2]) -> u64 {
    hay.chunks_exact(2)
        .filter(|p| p[0] == unit[0] && p[1] == unit[1])
        .count() as u64
}

#[cfg(target_arch = "x86_64")]
mod simd {
    use std::arch::x86_64::*;

    pub fn count_byte_sse2(hay: &[u8], needle: u8) -> u64 {
        const BLOCK: usize = 16 * 255;
        let mut blocks = hay.chunks_exact(BLOCK);
        let mut total = 0u64;
        // SAFETY: SSE2 is part of the x86_64 baseline; every load reads 16 bytes inside `block`.
        unsafe {
            let nv = _mm_set1_epi8(needle as i8);
            let zero = _mm_setzero_si128();
            let mut sums = _mm_setzero_si128();
            for block in &mut blocks {
                let p = block.as_ptr();
                let mut acc = _mm_setzero_si128();
                for i in 0..255 {
                    let x = _mm_loadu_si128(p.add(i * 16) as *const __m128i);
                    acc = _mm_sub_epi8(acc, _mm_cmpeq_epi8(x, nv));
                }
                sums = _mm_add_epi64(sums, _mm_sad_epu8(acc, zero));
            }
            let mut tail = blocks.remainder();
            let mut acc = _mm_setzero_si128();
            while tail.len() >= 16 {
                let x = _mm_loadu_si128(tail.as_ptr() as *const __m128i);
                acc = _mm_sub_epi8(acc, _mm_cmpeq_epi8(x, nv));
                tail = &tail[16..];
            }
            sums = _mm_add_epi64(sums, _mm_sad_epu8(acc, zero));
            let mut out = [0u64; 2];
            _mm_storeu_si128(out.as_mut_ptr() as *mut __m128i, sums);
            total += out[0] + out[1];
            total + tail.iter().filter(|&&b| b == needle).count() as u64
        }
    }

    pub fn count_u16_sse2(hay: &[u8], unit: [u8; 2]) -> u64 {
        // 16-bit counters, summed every 4096 rounds (well below i16::MAX for `madd`).
        const ROUNDS: usize = 4096;
        let mut blocks = hay.chunks_exact(16 * ROUNDS);
        let mut total = 0u64;
        // SAFETY: SSE2 is part of the x86_64 baseline; every load reads 16 bytes inside the slice.
        unsafe {
            let nv = _mm_set1_epi16(i16::from_le_bytes(unit));
            let ones = _mm_set1_epi16(1);
            let hsum = |acc: __m128i| -> u64 {
                let s = _mm_madd_epi16(acc, ones);
                let mut out = [0i32; 4];
                _mm_storeu_si128(out.as_mut_ptr() as *mut __m128i, s);
                out.iter().map(|&x| x as u64).sum()
            };
            for block in &mut blocks {
                let p = block.as_ptr();
                let mut acc = _mm_setzero_si128();
                for i in 0..ROUNDS {
                    let x = _mm_loadu_si128(p.add(i * 16) as *const __m128i);
                    acc = _mm_sub_epi16(acc, _mm_cmpeq_epi16(x, nv));
                }
                total += hsum(acc);
            }
            let mut tail = blocks.remainder();
            let mut acc = _mm_setzero_si128();
            while tail.len() >= 16 {
                let x = _mm_loadu_si128(tail.as_ptr() as *const __m128i);
                acc = _mm_sub_epi16(acc, _mm_cmpeq_epi16(x, nv));
                tail = &tail[16..];
            }
            total += hsum(acc);
            total + super::count_u16_portable(tail, unit)
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod simd {
    use std::arch::aarch64::*;

    pub fn count_byte_neon(hay: &[u8], needle: u8) -> u64 {
        const BLOCK: usize = 16 * 255;
        let mut blocks = hay.chunks_exact(BLOCK);
        let mut total = 0u64;
        // SAFETY: NEON is part of the aarch64 baseline; every load reads 16 bytes inside the slice.
        unsafe {
            let nv = vdupq_n_u8(needle);
            for block in &mut blocks {
                let p = block.as_ptr();
                let mut acc = vdupq_n_u8(0);
                for i in 0..255 {
                    acc = vsubq_u8(acc, vceqq_u8(vld1q_u8(p.add(i * 16)), nv));
                }
                total += vaddlvq_u8(acc) as u64;
            }
            let mut tail = blocks.remainder();
            let mut acc = vdupq_n_u8(0);
            while tail.len() >= 16 {
                acc = vsubq_u8(acc, vceqq_u8(vld1q_u8(tail.as_ptr()), nv));
                tail = &tail[16..];
            }
            total += vaddlvq_u8(acc) as u64;
            total + tail.iter().filter(|&&b| b == needle).count() as u64
        }
    }

    pub fn count_u16_neon(hay: &[u8], unit: [u8; 2]) -> u64 {
        const ROUNDS: usize = 4096;
        let mut blocks = hay.chunks_exact(16 * ROUNDS);
        let mut total = 0u64;
        // SAFETY: NEON is part of the aarch64 baseline; every load reads 16 bytes inside the slice.
        unsafe {
            let nv = vdupq_n_u16(u16::from_le_bytes(unit));
            for block in &mut blocks {
                let p = block.as_ptr();
                let mut acc = vdupq_n_u16(0);
                for i in 0..ROUNDS {
                    let x = vreinterpretq_u16_u8(vld1q_u8(p.add(i * 16)));
                    acc = vsubq_u16(acc, vceqq_u16(x, nv));
                }
                total += vaddlvq_u16(acc) as u64;
            }
            let mut tail = blocks.remainder();
            let mut acc = vdupq_n_u16(0);
            while tail.len() >= 16 {
                let x = vreinterpretq_u16_u8(vld1q_u8(tail.as_ptr()));
                acc = vsubq_u16(acc, vceqq_u16(x, nv));
                tail = &tail[16..];
            }
            total += vaddlvq_u16(acc) as u64;
            total + super::count_u16_portable(tail, unit)
        }
    }
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
        Codec {
            enc: Encoding::Utf8,
            nl: NlByte::Lf,
            bom_len: 0,
        }
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
                    if (base + p as u64).is_multiple_of(2) && p + 1 < hay.len() && hay[p + 1] == 0 {
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
                    if (base + p as u64).is_multiple_of(2) && p + 1 < hay.len() && hay[p + 1] == 0 {
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
            count_u16(&hay[start..], [u[0], u[1]])
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
                                let c =
                                    0x10000 + (((u as u32) - 0xD800) << 10) + ((l as u32) - 0xDC00);
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
                if (0xDC00..=0xDFFF).contains(&last)
                    && n >= 4
                    && (0xD800..=0xDBFF).contains(&rd(n - 4))
                {
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
                s.chars()
                    .map(|c| {
                        if (c as u32) < 0x80 {
                            c as u8
                        } else {
                            *rev.get(&c).unwrap_or(&b'?')
                        }
                    })
                    .collect()
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
    let bom_len = if sample.starts_with(enc.bom()) && !enc.bom().is_empty() {
        enc.bom().len()
    } else {
        bom_len.min(sample.len())
    };
    let content = &sample[bom_len.min(sample.len())..];
    let (nl, line_ending) = detect_line_endings(content, enc, whole_file);
    let looks_binary = looks_binary(content, enc);
    Detected {
        codec: Codec::new(enc, nl, bom_len),
        line_ending,
        looks_binary,
    }
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
        let nul_units = head
            .chunks_exact(2)
            .filter(|p| p[0] == 0 && p[1] == 0)
            .count();
        return units > 0 && nul_units * 100 > units;
    }
    let nul = count_byte(head, 0) as usize;
    let ctrl = head
        .iter()
        .filter(|&&b| b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r' | 0x0C | 0x1B))
        .count();
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
        let data: Vec<u8> = (0..100_003u32)
            .map(|i| {
                if i % 7 == 0 {
                    b'\n'
                } else {
                    (i % 200) as u8 + 20
                }
            })
            .collect();
        let naive = data.iter().filter(|&&b| b == b'\n').count() as u64;
        assert_eq!(count_byte(&data, b'\n'), naive);
        assert_eq!(
            count_byte(&data[..13], b'\n'),
            data[..13].iter().filter(|&&b| b == b'\n').count() as u64
        );
    }

    fn rand_bytes(n: usize, seed: u64, alphabet: &[u8]) -> Vec<u8> {
        let mut r = seed | 1;
        (0..n)
            .map(|_| {
                r ^= r << 13;
                r ^= r >> 7;
                r ^= r << 17;
                alphabet[(r % alphabet.len() as u64) as usize]
            })
            .collect()
    }

    #[test]
    fn count_byte_every_length_and_offset() {
        let data = rand_bytes(600, 7, b"ab\n\r\0\xff");
        for start in 0..17 {
            for end in start..data.len() {
                let s = &data[start..end];
                for needle in [b'\n', b'\r', 0, 0xff, b'z'] {
                    let naive = s.iter().filter(|&&b| b == needle).count() as u64;
                    assert_eq!(count_byte(s, needle), naive, "{start}..{end} {needle}");
                }
            }
        }
    }

    #[test]
    fn count_byte_large_and_saturated() {
        // Every byte a match: the per-lane counters reach their limit in every block.
        for n in [
            16 * 255 - 1,
            16 * 255,
            16 * 255 + 1,
            3 * 16 * 255 + 17,
            1 << 20,
        ] {
            let all = vec![b'\n'; n];
            assert_eq!(count_byte(&all, b'\n'), n as u64);
            assert_eq!(count_byte(&all, b'x'), 0);
        }
        let data = rand_bytes(5_000_003, 99, b"abcdefgh\n");
        let naive = data.iter().filter(|&&b| b == b'\n').count() as u64;
        assert_eq!(count_byte(&data, b'\n'), naive);
        assert_eq!(
            count_byte(&data[3..], b'\n'),
            count_byte_portable(&data[3..], b'\n')
        );
    }

    #[test]
    fn count_u16_matches_naive() {
        for seed in 1..4 {
            let data = rand_bytes(200_001, seed, b"\n\0\r ");
            for start in 0..5 {
                let ends = [
                    start,
                    start + 1,
                    start + 31,
                    16 * 4096 + 3,
                    16 * 4096 * 2 + 9,
                    data.len(),
                ];
                for end in ends {
                    let s = &data[start..end];
                    for unit in [*b"\n\0", *b"\0\n", *b"\r\0", *b"\0\0"] {
                        assert_eq!(
                            count_u16(s, unit),
                            count_u16_portable(s, unit),
                            "{seed} {start}..{end} {unit:?}"
                        );
                    }
                }
            }
        }
        let n = 16 * 4096 * 3 + 5;
        let all = b"\n\0".repeat(n);
        assert_eq!(count_u16(&all, *b"\n\0"), n as u64);
        assert_eq!(count_u16(&all, *b"\0\n"), 0);
        assert_eq!(count_u16(&all[1..], *b"\0\n"), n as u64 - 1);
    }

    #[test]
    fn count_nl_utf16_respects_alignment() {
        let text: String = (0..5000).map(|i| format!("рядок {i}\n")).collect();
        let le: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let be: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
        for (enc, data) in [(Encoding::Utf16Le, &le), (Encoding::Utf16Be, &be)] {
            let codec = Codec::new(enc, NlByte::Lf, 0);
            assert_eq!(codec.count_nl(data, 0), 5000);
            // At an odd absolute offset the first byte is the second half of a unit.
            assert_eq!(codec.count_nl(&data[1..], 1), 5000);
            for mid in [12_344, 12_346] {
                assert_eq!(
                    codec.count_nl(&data[..mid], 0) + codec.count_nl(&data[mid..], mid as u64),
                    5000,
                    "{mid}"
                );
            }
        }
    }

    #[test]
    fn detects_boms() {
        assert_eq!(
            detect_encoding(b"\xEF\xBB\xBFhi", true),
            (Encoding::Utf8, 3)
        );
        assert_eq!(
            detect_encoding(b"\xFF\xFEh\0i\0", true),
            (Encoding::Utf16Le, 2)
        );
        assert_eq!(
            detect_encoding(b"\xFE\xFF\0h\0i", true),
            (Encoding::Utf16Be, 2)
        );
    }

    #[test]
    fn detects_utf16_without_bom() {
        let le: Vec<u8> = "hello world\nsecond line\n"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        assert_eq!(detect_encoding(&le, true).0, Encoding::Utf16Le);
        let be: Vec<u8> = "hello world\nsecond line\n"
            .encode_utf16()
            .flat_map(|u| u.to_be_bytes())
            .collect();
        assert_eq!(detect_encoding(&be, true).0, Encoding::Utf16Be);
    }

    #[test]
    fn detects_utf8_and_single_byte() {
        assert_eq!(
            detect_encoding("привет мир\n".as_bytes(), true).0,
            Encoding::Utf8
        );
        let (cp1251, _, _) =
            encoding_rs::WINDOWS_1251.encode("Съешь же ещё этих мягких французских булок\n");
        assert_eq!(detect_encoding(&cp1251, true).0, Encoding::Windows1251);
        let (cp1252, _, _) =
            encoding_rs::WINDOWS_1252.encode("Le café est très bon à Paris, déjà vu.\n");
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
        assert_eq!(
            out,
            vec![REPLACEMENT, REPLACEMENT, 'a', REPLACEMENT, REPLACEMENT]
        );
        let u16c = Codec::new(Encoding::Utf16Le, NlByte::Lf, 0);
        assert_eq!(
            u16c.decode_char(&[0x00, 0xD8, 0x41, 0x00]),
            (REPLACEMENT, 2)
        );
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
