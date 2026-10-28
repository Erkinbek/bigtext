//! Log severity levels: the words that name them and a regex that finds them in a line.
//!
//! Upper-case level words (`ERROR`, `WARN`, …) count anywhere as whole words. Other spellings
//! count only inside the usual markers, so "no error found" is not an error line:
//! `[error]`, `<Error>`, `level=error`, `severity: Error` and JSON's `"level":"error"`.

use crate::search::Query;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
}

impl Level {
    pub const ALL: [Level; 4] = [Level::Error, Level::Warn, Level::Info, Level::Debug];

    pub fn name(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => "WARN",
            Level::Info => "INFO",
            Level::Debug => "DEBUG",
        }
    }

    /// The words that mark a line of this level.
    pub fn words(self) -> &'static [&'static str] {
        match self {
            Level::Error => &["ERROR", "ERR", "FATAL", "CRITICAL", "CRIT", "SEVERE", "EMERG", "EMERGENCY", "ALERT", "PANIC"],
            Level::Warn => &["WARN", "WARNING"],
            Level::Info => &["INFO", "NOTICE"],
            Level::Debug => &["DEBUG", "TRACE", "VERBOSE", "FINE", "FINER", "FINEST"],
        }
    }

    fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// A set of levels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Levels(u8);

impl Levels {
    pub fn only(l: Level) -> Levels {
        Levels(l.bit())
    }

    pub fn contains(self, l: Level) -> bool {
        self.0 & l.bit() != 0
    }

    pub fn toggled(self, l: Level) -> Levels {
        Levels(self.0 ^ l.bit())
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The set as a number (for saving).
    pub fn bits(self) -> u8 {
        self.0
    }

    pub fn from_bits(b: u8) -> Levels {
        Levels(b & 0x0F)
    }

    pub fn iter(self) -> impl Iterator<Item = Level> {
        Level::ALL.into_iter().filter(move |&l| self.contains(l))
    }

    /// "ERROR, WARN".
    pub fn names(self) -> String {
        self.iter().map(Level::name).collect::<Vec<_>>().join(", ")
    }
}

/// Regex that matches a level word of `levels` in a line (None for an empty set).
pub fn pattern(levels: Levels) -> Option<String> {
    let words: Vec<&str> = levels.iter().flat_map(|l| l.words().iter().copied()).collect();
    if words.is_empty() {
        return None;
    }
    let w = words.join("|");
    // ASCII word boundaries keep the regex on its fast engines.
    Some(format!(
        r#"(?-u:\b)(?:{w})(?-u:\b)|[\[<](?i-u:{w})[\]>]|(?i-u:\b(?:level|severity|lvl|loglevel)\b)["']?[ \t]*[:=][ \t]*["']?(?i-u:{w})(?-u:\b)"#
    ))
}

/// Search query for the lines of `levels`.
pub fn query(levels: Levels) -> Option<Query> {
    Some(Query { pattern: pattern(levels)?, regex: true, case_sensitive: true, whole_word: false, dot_all: false })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::Codec;
    use crate::search::Matcher;

    fn hit(levels: Levels, line: &str) -> bool {
        let m = Matcher::new(&query(levels).unwrap(), Codec::utf8()).unwrap();
        let mut found = false;
        m.find_in(line.as_bytes(), 0, 0, line.len(), line.len(), &mut None, &mut |_, _| {
            found = true;
            false
        });
        found
    }

    #[test]
    fn finds_the_usual_spellings() {
        let e = Levels::only(Level::Error);
        for line in [
            "2024-05-01 10:00:00 ERROR db: timeout",
            "2024-05-01 10:00:00 [ERROR] x",
            "[error] 1234#0: *1 open() failed",
            "<Error> something",
            "ts=1 level=error msg=boom",
            "ts=1 level=\"error\" msg=boom",
            r#"{"ts":1,"level":"error","msg":"boom"}"#,
            r#"{"severity": "Error"}"#,
            "E FATAL: out of memory",
            "Jan  1 00:00:00 host kernel: CRIT disk",
        ] {
            assert!(hit(e, line), "{line}");
        }
    }

    #[test]
    fn ignores_ordinary_words() {
        let e = Levels::only(Level::Error);
        for line in ["no error found", "ERRORS: 0", "an Error occurred", "level=errorless", "TERROR", "MyError: x", "INFO all good"] {
            assert!(!hit(e, line), "{line}");
        }
    }

    #[test]
    fn sets_combine() {
        let ew = Levels::only(Level::Error).toggled(Level::Warn);
        assert!(hit(ew, "WARN low disk"));
        assert!(hit(ew, "WARNING low disk"));
        assert!(hit(ew, "ERROR x"));
        assert!(!hit(ew, "INFO x"));
        assert_eq!(ew.names(), "ERROR, WARN");
        assert!(ew.toggled(Level::Error).toggled(Level::Warn).is_empty());
        assert_eq!(pattern(Levels::default()), None);
        assert!(hit(Levels::only(Level::Debug), "2024 TRACE enter"));
        assert!(hit(Levels::only(Level::Info), "[info] started"));
    }
}
