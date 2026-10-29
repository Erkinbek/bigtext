//! What the Go to dialog takes: a line (1234), lines from the caret (+100, -100), lines before
//! the end ($ is the last line, $-100), a share of the file (50%), and byte offsets (decimal or
//! 0x hex, also relative, from the end or as a share).

use super::document::Document;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GotoTarget {
    /// 1-based.
    Line(u64),
    LinesFrom(i64),
    /// Lines before the last one.
    FromEnd(u64),
    /// Of the file's bytes, 0 to 100.
    Percent(f64),
    Offset(u64),
    OffsetFrom(i64),
    OffsetFromEnd(u64),
}

fn number(t: &str, hex: bool) -> Option<u64> {
    match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(h) if hex => u64::from_str_radix(h, 16).ok(),
        Some(_) => None,
        None => t.parse().ok(),
    }
}

/// Parses `text` for the Line (`by_offset` false) or Byte offset mode.
pub fn parse(text: &str, by_offset: bool) -> Option<GotoTarget> {
    let t: String = text.chars().filter(|c| !matches!(c, '_' | ',' | ' ')).collect();
    if let Some(p) = t.strip_suffix('%') {
        let v: f64 = p.parse().ok()?;
        return (0.0..=100.0).contains(&v).then_some(GotoTarget::Percent(v));
    }
    if let Some(rest) = t.strip_prefix('$') {
        let n = if rest.is_empty() { 0 } else { number(rest.strip_prefix('-')?, by_offset)? };
        return Some(if by_offset { GotoTarget::OffsetFromEnd(n) } else { GotoTarget::FromEnd(n) });
    }
    let signed = |s: &str| -> Option<i64> { i64::try_from(number(s, by_offset)?).ok() };
    if let Some(rest) = t.strip_prefix('+') {
        let n = signed(rest)?;
        return Some(if by_offset { GotoTarget::OffsetFrom(n) } else { GotoTarget::LinesFrom(n) });
    }
    if let Some(rest) = t.strip_prefix('-') {
        let n = -signed(rest)?;
        return Some(if by_offset { GotoTarget::OffsetFrom(n) } else { GotoTarget::LinesFrom(n) });
    }
    let n = number(&t, true)?;
    Some(if by_offset { GotoTarget::Offset(n) } else { GotoTarget::Line(n) })
}

impl Document {
    pub fn goto_target(&mut self, t: GotoTarget) {
        let len = self.buf.len();
        match t {
            GotoTarget::Line(n) => self.goto_line(n),
            GotoTarget::LinesFrom(d) => {
                let (cur, exact) = self.buf.line_of(self.view.caret);
                self.goto_line((cur as i64 + d).max(0) as u64 + 1);
                // From an estimated line, a later refinement would move the caret elsewhere.
                if !exact {
                    self.view.pending_goto_line = None;
                }
            }
            GotoTarget::FromEnd(n) => {
                let (count, exact) = self.buf.line_count();
                self.goto_line(count.saturating_sub(n).max(1));
                if !exact {
                    self.view.pending_goto_line = None;
                }
            }
            GotoTarget::Percent(p) => {
                let off = ((len as f64) * p / 100.0) as u64;
                let off = bigtext::text::rows::seg_start(&self.buf, off.min(len)).unwrap_or(off);
                self.goto_offset(off);
            }
            GotoTarget::Offset(o) => self.goto_offset(o),
            GotoTarget::OffsetFrom(d) => self.goto_offset((self.view.caret as i64 + d).max(0) as u64),
            GotoTarget::OffsetFromEnd(n) => self.goto_offset(len.saturating_sub(n)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use GotoTarget::*;

    #[test]
    fn forms() {
        assert_eq!(parse("1,234", false), Some(Line(1234)));
        assert_eq!(parse("0x10", false), Some(Line(16)));
        assert_eq!(parse("+100", false), Some(LinesFrom(100)));
        assert_eq!(parse("-25", false), Some(LinesFrom(-25)));
        assert_eq!(parse("$", false), Some(FromEnd(0)));
        assert_eq!(parse("$-100", false), Some(FromEnd(100)));
        assert_eq!(parse("50%", false), Some(Percent(50.0)));
        assert_eq!(parse("12.5 %", true), Some(Percent(12.5)));
        assert_eq!(parse("0x1F", true), Some(Offset(31)));
        assert_eq!(parse("+0x10", true), Some(OffsetFrom(16)));
        assert_eq!(parse("-4096", true), Some(OffsetFrom(-4096)));
        assert_eq!(parse("$-0x100", true), Some(OffsetFromEnd(256)));
        assert_eq!(parse("+0x10", false), None);
        assert_eq!(parse("150%", false), None);
        assert_eq!(parse("$5", false), None);
        assert_eq!(parse("abc", false), None);
        assert_eq!(parse("", false), None);
    }
}
