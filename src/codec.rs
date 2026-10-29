//! Decoding and encoding text the way logs and requests carry it: Base64 (and JWTs), URL
//! percent-encoding, hex, HTML entities and JSON string escapes.

/// Base64 with padding, standard alphabet.
pub fn b64_encode(b: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Base64 in either alphabet (`+/` or `-_`), with or without padding; white space is skipped.
pub fn b64_decode(s: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for (i, c) in s.trim_end_matches('=').chars().enumerate() {
        let v = match c {
            'A'..='Z' => c as u32 - 'A' as u32,
            'a'..='z' => c as u32 - 'a' as u32 + 26,
            '0'..='9' => c as u32 - '0' as u32 + 52,
            '+' | '-' => 62,
            '/' | '_' => 63,
            c if c.is_whitespace() => continue,
            _ => return Err(format!("“{c}” (character {}) is not Base64.", i + 1)),
        };
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    if bits >= 6 {
        return Err("The Base64 text is cut short.".into());
    }
    Ok(out)
}

/// A JSON Web Token's header and payload, pretty-printed, when `s` is one.
pub fn jwt(s: &str) -> Option<String> {
    let parts: Vec<&str> = s.trim().split('.').collect();
    if parts.len() != 3 || parts[0].is_empty() || parts[1].is_empty() {
        return None;
    }
    let part = |p: &str| -> Option<String> {
        let text = String::from_utf8(b64_decode(p).ok()?).ok()?;
        text.trim_start().starts_with('{').then(|| crate::textops::json_format(&text, Some("  "), "\n").unwrap_or(text))
    };
    let (header, payload) = (part(parts[0])?, part(parts[1])?);
    let sig = b64_decode(parts[2]).map(|b| b.len()).unwrap_or(0);
    Some(format!("header:\n{header}\n\npayload:\n{payload}\n\nsignature: {sig} bytes (not checked)\n"))
}

fn hex_digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// `%xx` for every byte but the unreserved ones (`A-Z a-z 0-9 - _ . ~`).
pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `%xx` back to bytes; `+` is a space in a form (`a=b&c=d`) only.
pub fn url_decode(s: &str) -> Result<Vec<u8>, String> {
    let form = s.contains('=') && !s.contains(' ');
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let (Some(h), Some(l)) = (b.get(i + 1).copied().and_then(hex_digit), b.get(i + 2).copied().and_then(hex_digit)) else {
                    return Err(format!("“%” at character {} is not followed by two hex digits.", i + 1));
                };
                out.push(h << 4 | l);
                i += 3;
            }
            b'+' if form => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    Ok(out)
}

pub fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Hex digits to bytes; spaces, `:` and `-` between them and `0x` prefixes are skipped.
pub fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    let mut digits = Vec::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'0' && matches!(b.get(i + 1), Some(b'x' | b'X')) {
            i += 2;
            continue;
        }
        match hex_digit(c) {
            Some(d) => digits.push(d),
            None if c.is_ascii_whitespace() || c == b':' || c == b'-' || c == b',' => {}
            None => return Err(format!("“{}” (character {}) is not a hex digit.", c as char, i + 1)),
        }
        i += 1;
    }
    if digits.len() % 2 == 1 {
        return Err("An odd number of hex digits.".into());
    }
    Ok(digits.chunks(2).map(|p| p[0] << 4 | p[1]).collect())
}

pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

const ENTITIES: [(&str, char); 32] = [
    ("amp", '&'),
    ("lt", '<'),
    ("gt", '>'),
    ("quot", '"'),
    ("apos", '\''),
    ("nbsp", '\u{a0}'),
    ("copy", '©'),
    ("reg", '®'),
    ("trade", '™'),
    ("hellip", '…'),
    ("mdash", '—'),
    ("ndash", '–'),
    ("lsquo", '‘'),
    ("rsquo", '’'),
    ("ldquo", '“'),
    ("rdquo", '”'),
    ("laquo", '«'),
    ("raquo", '»'),
    ("bull", '•'),
    ("middot", '·'),
    ("deg", '°'),
    ("plusmn", '±'),
    ("times", '×'),
    ("divide", '÷'),
    ("euro", '€'),
    ("pound", '£'),
    ("yen", '¥'),
    ("cent", '¢'),
    ("sect", '§'),
    ("para", '¶'),
    ("larr", '←'),
    ("rarr", '→'),
];

/// Named (the common ones) and numeric entities back to characters; unknown ones stay.
pub fn html_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest[1..].find(';').map(|e| e + 1).filter(|&e| e <= 12);
        let decoded = end.and_then(|e| {
            let name = &rest[1..e];
            let c = if let Some(n) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                u32::from_str_radix(n, 16).ok().and_then(char::from_u32)
            } else if let Some(n) = name.strip_prefix('#') {
                n.parse().ok().and_then(char::from_u32)
            } else {
                ENTITIES.iter().find(|(k, _)| *k == name).map(|x| x.1)
            };
            c.map(|c| (c, e + 1))
        });
        match decoded {
            Some((c, n)) => {
                out.push(c);
                rest = &rest[n..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// A JSON string's content: quotes, backslashes and control characters escaped.
pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// A JSON string's escapes undone (its quotes dropped, when they are there).
pub fn json_unescape(s: &str) -> Result<String, String> {
    let s = s.strip_prefix('"').and_then(|x| x.strip_suffix('"')).unwrap_or(s);
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    let hex4 = |it: &mut std::str::Chars| -> Result<u32, String> {
        let h: String = it.by_ref().take(4).collect();
        u32::from_str_radix(&h, 16).ok().filter(|_| h.len() == 4).ok_or_else(|| format!("“\\u{h}” is not four hex digits."))
    };
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('u') => {
                let mut v = hex4(&mut it)?;
                // A surrogate pair.
                if (0xD800..0xDC00).contains(&v) {
                    let rest = it.as_str();
                    if let Some(low) = rest.strip_prefix("\\u") {
                        if let Ok(lo) = u32::from_str_radix(low.get(..4).unwrap_or(""), 16) {
                            if (0xDC00..0xE000).contains(&lo) {
                                v = 0x10000 + ((v - 0xD800) << 10) + (lo - 0xDC00);
                                it = low[4..].chars();
                            }
                        }
                    }
                }
                out.push(char::from_u32(v).unwrap_or('\u{fffd}'));
            }
            Some(c @ ('"' | '\\' | '/')) => out.push(c),
            Some(c) => return Err(format!("“\\{c}” is not a JSON escape.")),
            None => return Err("The text ends with a lone backslash.".into()),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_both_alphabets_and_jwts() {
        assert_eq!(b64_encode(b"hello world"), "aGVsbG8gd29ybGQ=");
        assert_eq!(b64_encode(b""), "");
        assert_eq!(b64_encode(b"ab"), "YWI=");
        assert_eq!(b64_decode("aGVsbG8gd29ybGQ=").unwrap(), b"hello world");
        assert_eq!(b64_decode("aGVsbG8gd29ybGQ").unwrap(), b"hello world");
        assert_eq!(b64_decode("-_-_").unwrap(), b64_decode("+/+/").unwrap());
        assert!(b64_decode("abc$").is_err());
        assert!(b64_decode("a").is_err());
        let token = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjMiLCJuYW1lIjoiQW5uIn0.c2lnbmF0dXJl";
        let shown = jwt(token).unwrap();
        assert!(shown.contains("\"alg\": \"HS256\"") && shown.contains("\"name\": \"Ann\"") && shown.contains("signature: 9 bytes"), "{shown}");
        assert_eq!(jwt("a.b"), None);
    }

    #[test]
    fn url_hex_html_json() {
        assert_eq!(url_encode("a b&c=d/é"), "a%20b%26c%3Dd%2F%C3%A9");
        assert_eq!(url_decode("a%20b%26c%3Dd%2F%C3%A9").unwrap(), "a b&c=d/é".as_bytes());
        assert_eq!(url_decode("q=a+b&x=1").unwrap(), b"q=a b&x=1");
        assert_eq!(url_decode("1+1").unwrap(), b"1+1");
        assert!(url_decode("%zz").is_err());
        assert_eq!(hex_encode(b"Hi\x00"), "486900");
        assert_eq!(hex_decode("48 69 0x00").unwrap(), b"Hi\x00");
        assert_eq!(hex_decode("48:69").unwrap(), b"Hi");
        assert!(hex_decode("486").is_err());
        assert_eq!(html_escape("<a href=\"x\">&</a>"), "&lt;a href=&quot;x&quot;&gt;&amp;&lt;/a&gt;");
        assert_eq!(html_unescape("&lt;b&gt; &amp; &#65;&#x42; &copy; &bogus; & x"), "<b> & AB © &bogus; & x");
        assert_eq!(json_escape("a\"b\\c\nd\u{1}"), "a\\\"b\\\\c\\nd\\u0001");
        assert_eq!(json_unescape("\"a\\\"b\\\\c\\nd\\u0041\\ud83d\\ude00\"").unwrap(), "a\"b\\c\ndA😀");
        assert!(json_unescape("\\q").is_err());
    }
}
