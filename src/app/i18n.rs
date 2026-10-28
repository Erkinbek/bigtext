//! Interface languages. The English text in the code is the key: [`tr`] returns its translation
//! for the chosen language, or the English text when there is none.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    En,
    Ru,
    Uz,
}

impl Lang {
    pub const ALL: [Lang; 3] = [Lang::En, Lang::Ru, Lang::Uz];

    /// Native name, shown in the language menu.
    pub fn name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Ru => "Русский",
            Lang::Uz => "O'zbekcha",
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Ru => "ru",
            Lang::Uz => "uz",
        }
    }

    pub fn from_code(s: &str) -> Option<Lang> {
        Lang::ALL.into_iter().find(|l| l.code() == s)
    }
}

static LANG: AtomicU8 = AtomicU8::new(0);

pub fn set_lang(l: Lang) {
    LANG.store(l as u8, Ordering::Relaxed);
}

pub fn lang() -> Lang {
    match LANG.load(Ordering::Relaxed) {
        1 => Lang::Ru,
        2 => Lang::Uz,
        _ => Lang::En,
    }
}

fn table(l: Lang) -> Option<&'static HashMap<&'static str, &'static str>> {
    static RU: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    static UZ: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    let build = |col: usize| TEXTS.iter().map(|t| (t[0], t[col])).filter(|(_, v)| !v.is_empty()).collect();
    match l {
        Lang::En => None,
        Lang::Ru => Some(RU.get_or_init(|| build(1))),
        Lang::Uz => Some(UZ.get_or_init(|| build(2))),
    }
}

/// The interface text `s` in the current language.
pub fn tr(s: &'static str) -> &'static str {
    match table(lang()) {
        Some(t) => t.get(s).copied().unwrap_or(s),
        None => s,
    }
}

/// [`tr`] with `{name}` placeholders filled in.
pub fn trf(s: &'static str, args: &[(&str, &str)]) -> String {
    let mut out = tr(s).to_string();
    for (k, v) in args {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

/// English, Russian, Uzbek.
static TEXTS: &[[&str; 3]] = &[];
