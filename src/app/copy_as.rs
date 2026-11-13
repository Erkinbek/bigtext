//! Edit → Copy as ▸ Markdown (a fenced code block with the language and `file:line`) or HTML
//! (the theme's syntax colours), for pasting into chats, wikis, mail and documents.

use crate::view::highlight::{self, Lang};

use super::i18n::tr;
use super::BigTextApp;

/// The selection copied at most.
const COPY_AS_MAX: u64 = 4 << 20;
/// HTML is several times the text: smaller selections only.
const HTML_MAX: usize = 2 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyAs {
    Markdown,
    Html,
}

/// The fence's language tag.
pub fn fence_lang(lang: Lang) -> &'static str {
    match lang {
        Lang::Plain => "",
        Lang::Log => "log",
        Lang::Json => "json",
        Lang::Xml => "xml",
        Lang::Csv(b'\t') => "tsv",
        Lang::Csv(_) => "csv",
        Lang::Sql => "sql",
        Lang::Ini => "ini",
        Lang::C => "cpp",
        Lang::CSharp => "csharp",
        Lang::Java => "java",
        Lang::JavaScript => "javascript",
        Lang::TypeScript => "typescript",
        Lang::Go => "go",
        Lang::Rust => "rust",
        Lang::Php => "php",
        Lang::Python => "python",
        Lang::Shell => "bash",
        Lang::Kotlin => "kotlin",
        Lang::Swift => "swift",
        Lang::Dart => "dart",
        Lang::Ruby => "ruby",
        Lang::Lua => "lua",
        Lang::PowerShell => "powershell",
        Lang::Css => "css",
        Lang::Markdown => "markdown",
        Lang::Yaml => "yaml",
        Lang::Dockerfile => "dockerfile",
        Lang::Makefile => "makefile",
    }
}

/// `text` in a fenced block (longer than any run of backticks in it), after its caption.
pub fn markdown_block(text: &str, lang: Lang, caption: Option<&str>) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat((longest + 1).max(3));
    let body = text.trim_end_matches(['\n', '\r']);
    let caption = caption.map(|c| format!("{c}\n")).unwrap_or_default();
    format!("{caption}{fence}{}\n{body}\n{fence}\n", fence_lang(lang))
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn hex(c: egui::Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

/// `text` as preformatted HTML with each token in its syntax colour; `st` is what the text starts
/// inside of (a comment opened above the selection).
pub fn html_block(
    text: &str,
    lang: Lang,
    st: highlight::State,
    dark: bool,
    caption: Option<&str>,
) -> String {
    // Code takes the code colours (their palette may be dark on a light interface).
    let code_dark = lang.is_code().then(|| super::theme::code_colors(dark).dark);
    let (bg, fg) = if lang.is_code() {
        let c = super::theme::code_colors(dark);
        (hex(c.bg), hex(c.fg))
    } else if dark {
        ("#1e1e1e".into(), "#d4d4d4".into())
    } else {
        ("#f6f8fa".into(), "#24292f".into())
    };
    let mut out = String::from("<div>");
    if let Some(c) = caption {
        out.push_str(&format!(
            "<div style=\"font-family:sans-serif;font-size:90%;color:#888\">{}</div>",
            escape(c)
        ));
    }
    out.push_str(&format!("<pre style=\"font-family:Consolas,'Courier New',monospace;background:{bg};color:{fg};padding:8px;margin:0\">"));
    let mut spans = Vec::new();
    let mut st = st;
    for (i, line) in text.trim_end_matches(['\n', '\r']).split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let line = line.strip_suffix('\r').unwrap_or(line);
        spans.clear();
        st = highlight::spans_from(lang, st, line, true, &mut spans);
        let mut at = 0;
        for &(s, e, kind) in &spans {
            if s < at || e > line.len() || !line.is_char_boundary(s) || !line.is_char_boundary(e) {
                continue;
            }
            out.push_str(&escape(&line[at..s]));
            let color = code_dark
                .and_then(|d| super::theme::code_token(kind, d))
                .unwrap_or_else(|| highlight::color(kind, dark));
            out.push_str(&format!(
                "<span style=\"color:{}\">{}</span>",
                hex(color),
                escape(&line[s..e])
            ));
            at = e;
        }
        out.push_str(&escape(&line[at..]));
    }
    out.push_str("</pre></div>");
    out
}

/// Puts HTML (with the plain text beside it) on the system clipboard.
fn copy_html(html: &str, text: &str) -> Result<(), String> {
    let mut c = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    c.set().html(html, Some(text)).map_err(|e| e.to_string())
}

impl BigTextApp {
    /// Copies the selection as `how`.
    pub(crate) fn copy_as(&mut self, ctx: &egui::Context, how: CopyAs) {
        let Some(d) = self.docs.get(self.active) else {
            return;
        };
        let (s, e) = d.selection();
        if s == e {
            self.notify(tr("Select the text to copy.").into(), None);
            return;
        }
        let text = match d.selection_text(COPY_AS_MAX) {
            Some(Ok(t)) => t,
            Some(Err(_)) => {
                self.error =
                    Some(tr("The selection is too large to copy this way (over 4 MB).").into());
                return;
            }
            None => return,
        };
        let lang = if self.settings.syntax {
            d.effective_lang()
        } else {
            Lang::Plain
        };
        let name = d
            .unpacked_from
            .as_deref()
            .or(d.path.as_deref())
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| d.title.clone());
        let caption = format!("{name}:{}", d.buf.line_of(s).0 + 1);
        match how {
            CopyAs::Markdown => ctx.copy_text(markdown_block(&text, lang, Some(&caption))),
            CopyAs::Html => {
                if text.len() > HTML_MAX {
                    self.error =
                        Some(tr("Copy as HTML works on up to 2 MB. Select a smaller part.").into());
                    return;
                }
                let st = crate::view::hl_rows::state_at(&d.buf, lang, s).unwrap_or_default();
                let html = html_block(
                    &text,
                    lang,
                    st,
                    self.settings.theme.is_dark(),
                    Some(&caption),
                );
                if let Err(e) = copy_html(&html, &text) {
                    ctx.copy_text(text);
                    self.notify(
                        format!(
                            "{} ({e})",
                            tr("HTML could not be put on the clipboard; copied as plain text")
                        ),
                        None,
                    );
                    return;
                }
            }
        }
        self.notify(tr("Copied.").into(), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_fences_and_html_colours() {
        assert_eq!(
            markdown_block("a\nb\n", Lang::Log, Some("app.log:3")),
            "app.log:3\n```log\na\nb\n```\n"
        );
        assert_eq!(
            markdown_block("x ``` y", Lang::Plain, None),
            "````\nx ``` y\n````\n"
        );
        let html = html_block(
            "2024-05-01 10:00:00 ERROR <boom> & co\n",
            Lang::Log,
            Default::default(),
            false,
            Some("a.log:1"),
        );
        assert!(
            html.contains("&lt;boom&gt; &amp; co")
                && html.contains("<span style=\"color:#")
                && html.starts_with("<div>"),
            "{html}"
        );
        assert!(!html.contains("\n</pre>"));
        // A comment over lines is one colour on each of them.
        let html = html_block(
            "/* a\nselect */ select",
            Lang::Sql,
            Default::default(),
            true,
            None,
        );
        let grey = hex(highlight::color(highlight::Kind::Comment, true));
        assert!(
            html.contains(&format!("<span style=\"color:{grey}\">select */</span>")),
            "{html}"
        );
    }
}
