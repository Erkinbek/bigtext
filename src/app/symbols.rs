//! Search → Go to symbol (Ctrl+R): the functions, classes and types defined in a code file,
//! found line by line on a worker (comments and strings are blanked first, using the
//! highlighter's state, so a `def` in a docstring is not one), filtered by a fuzzy match of the
//! name. At most [`SCAN_MAX`] of the file is read.

use std::sync::mpsc;
use std::sync::OnceLock;

use egui::{Key, Modifiers};
use regex_automata::meta::Regex;

use super::i18n::{tr, trf};
use super::BigTextApp;
use crate::view::highlight::{self, Kind, Lang};

/// How much of a file is read for its symbols.
pub const SCAN_MAX: u64 = 32 * 1024 * 1024;
const SHOWN: usize = 300;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymKind {
    Function,
    Class,
    Type,
    Macro,
}

impl SymKind {
    fn badge(self) -> &'static str {
        match self {
            SymKind::Function => "ƒ",
            SymKind::Class => "C",
            SymKind::Type => "T",
            SymKind::Macro => "#",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub kind: SymKind,
    /// 0-based line.
    pub line: u64,
    /// Indentation level (methods under their class).
    pub depth: u8,
}

struct Rule {
    re: Regex,
    kind: SymKind,
    /// Put before the name ("impl ").
    prefix: &'static str,
}

/// Words that look like a function name before `(` but are not one.
const NOT_NAMES: &[&str] = &["if", "for", "foreach", "while", "switch", "catch", "return", "new", "delete", "throw", "sizeof", "else", "case", "do", "using", "typeof", "await", "elif", "with", "match", "function", "lock", "fixed", "when", "assert", "defined"];

fn rules(lang: Lang) -> &'static [Rule] {
    fn build(list: &[(&str, SymKind, &'static str)]) -> Vec<Rule> {
        list.iter().map(|&(p, kind, prefix)| Rule { re: Regex::new(p).expect("symbol pattern"), kind, prefix }).collect()
    }
    use SymKind::*;
    const ID: &str = r"[A-Za-z_][A-Za-z0-9_]*";
    static CELLS: OnceLock<Vec<(Lang, Vec<Rule>)>> = OnceLock::new();
    let all = CELLS.get_or_init(|| {
        let id = |s: &str| s.replace("ID", ID);
        let c_func = id(r"^\s*(?:[\w:<>,\*&~\[\]]+\s+)+[\*&]*(~?[A-Za-z_][\w:]*)\s*\(");
        let c_types = id(r"^\s*(?:(?:public|private|protected|internal|static|abstract|sealed|final|partial|readonly|export|typedef)\s+)*(?:class|struct|union|interface|enum(?:\s+class)?|record|namespace)\s+(ID)");
        vec![
            (Lang::Python, build(&[(&id(r"^\s*(?:async\s+)?def\s+(ID)"), Function, ""), (&id(r"^\s*class\s+(ID)"), Class, "")])),
            (
                Lang::JavaScript,
                build(&[
                    (r"^\s*(?:export\s+)?(?:default\s+)?(?:async\s+)?function\s*\*?\s*([A-Za-z_$][\w$]*)", Function, ""),
                    (r"^\s*(?:export\s+)?(?:default\s+)?(?:abstract\s+)?class\s+([A-Za-z_$][\w$]*)", Class, ""),
                    (r"^\s*(?:export\s+)?(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*(?::[^=]+)?=\s*(?:async\s+)?(?:function\b|\([^)]*\)\s*(?::[^=]+)?=>|[A-Za-z_$][\w$]*\s*=>)", Function, ""),
                    (r"^\s+(?:(?:public|private|protected|static|readonly|async|override|get|set)\s+)*([A-Za-z_$][\w$]*)\s*(?:<[^>]*>)?\s*\([^)]*\)\s*(?::\s*[^{=;]+)?\s*\{\s*$", Function, ""),
                ]),
            ),
            (
                Lang::TypeScript,
                build(&[
                    (r"^\s*(?:export\s+)?(?:default\s+)?(?:async\s+)?function\s*\*?\s*([A-Za-z_$][\w$]*)", Function, ""),
                    (r"^\s*(?:export\s+)?(?:default\s+)?(?:abstract\s+)?class\s+([A-Za-z_$][\w$]*)", Class, ""),
                    (r"^\s*(?:export\s+)?(?:declare\s+)?(?:interface|enum|type)\s+([A-Za-z_$][\w$]*)", Type, ""),
                    (r"^\s*(?:export\s+)?(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*(?::[^=]+)?=\s*(?:async\s+)?(?:function\b|\([^)]*\)\s*(?::[^=]+)?=>|[A-Za-z_$][\w$]*\s*=>)", Function, ""),
                    (r"^\s+(?:(?:public|private|protected|static|readonly|async|override|get|set|abstract)\s+)*([A-Za-z_$][\w$]*)\s*(?:<[^>]*>)?\s*\([^)]*\)\s*(?::\s*[^{=;]+)?\s*\{\s*$", Function, ""),
                ]),
            ),
            (Lang::Go, build(&[(&id(r"^func\s+(?:\([^)]*\)\s*)?(ID)"), Function, ""), (&id(r"^type\s+(ID)"), Type, "")])),
            (
                Lang::Rust,
                build(&[
                    (&id(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:(?:const|async|unsafe|extern|default)\s+)*fn\s+(ID)"), Function, ""),
                    (&id(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum|trait|union|type|mod)\s+(ID)"), Type, ""),
                    (r"^\s*(?:unsafe\s+)?impl(?:<[^{]*?>)?\s+([^{]+?)\s*(?:\{|\bwhere\b|$)", Class, "impl "),
                    (&id(r"^\s*macro_rules!\s*(ID)"), Macro, ""),
                ]),
            ),
            (
                Lang::Php,
                build(&[
                    (&id(r"^\s*(?:(?:abstract|final|public|private|protected|static|readonly)\s+)*function\s+&?\s*(ID)"), Function, ""),
                    (&id(r"^\s*(?:(?:abstract|final|readonly)\s+)*(?:class|interface|trait|enum)\s+(ID)"), Class, ""),
                ]),
            ),
            (Lang::Shell, build(&[(r"^\s*function\s+([A-Za-z_][\w.-]*)", Function, ""), (r"^\s*([A-Za-z_][\w.-]*)\s*\(\s*\)", Function, "")])),
            (Lang::C, build(&[(&c_types, Class, ""), (&id(r"^#\s*define\s+(ID)"), Macro, ""), (&c_func, Function, "")])),
            (Lang::CSharp, build(&[(&c_types, Class, ""), (&c_func, Function, "")])),
            (
                Lang::Kotlin,
                build(&[
                    (&id(r"^\s*(?:(?:public|private|protected|internal|open|override|abstract|suspend|inline|operator|infix|tailrec|external|actual|expect)\s+)*fun\s+(?:<[^>]*>\s*)?(?:[\w.]+\.)?(ID)"), Function, ""),
                    (&id(r"^\s*(?:(?:public|private|protected|internal|open|abstract|data|sealed|enum|annotation|inner|value|companion)\s+)*(?:class|interface|object|typealias)\s+(ID)"), Class, ""),
                ]),
            ),
            (
                Lang::Swift,
                build(&[
                    (&id(r"^\s*(?:(?:public|private|fileprivate|internal|open|static|class|final|override|mutating|nonisolated|@\w+)\s+)*func\s+(ID)"), Function, ""),
                    (&id(r"^\s*(?:(?:public|private|fileprivate|internal|open|final|indirect|@\w+)\s+)*(?:class|struct|enum|protocol|extension|actor|typealias)\s+(ID)"), Class, ""),
                ]),
            ),
            (Lang::Ruby, build(&[(r"^\s*def\s+(?:self\.)?([A-Za-z_]\w*[?!=]?)", Function, ""), (r"^\s*(?:class|module)\s+([A-Z][\w:]*)", Class, "")])),
            (Lang::Lua, build(&[(r"^\s*(?:local\s+)?function\s+([\w.:]+)", Function, ""), (r"^\s*(?:local\s+)?([\w.]+)\s*=\s*function\b", Function, "")])),
            (Lang::PowerShell, build(&[(r"(?i)^\s*(?:function|filter|workflow)\s+([\w-]+)", Function, ""), (r"(?i)^\s*class\s+(\w+)", Class, ""), (r"(?i)^\s*enum\s+(\w+)", Type, "")])),
            (Lang::Dart, build(&[(&id(r"^\s*(?:(?:abstract|sealed|base|final|interface)\s+)*(?:class|mixin|enum|extension|typedef)\s+(ID)"), Class, ""), (&c_func, Function, "")])),
            (Lang::Java, build(&[(&c_types, Class, ""), (&c_func, Function, "")])),
        ]
    });
    all.iter().find(|(l, _)| *l == lang).map_or(&[], |(_, r)| r.as_slice())
}

/// True if `lang` has symbols to list.
pub fn supported(lang: Lang) -> bool {
    !rules(lang).is_empty()
}

/// The symbols of `text` (code in `lang`), with depths from the indentation step `unit`.
pub fn scan(lang: Lang, text: &str, unit: u32) -> Vec<Symbol> {
    let rules = rules(lang);
    let mut out = Vec::new();
    if rules.is_empty() {
        return out;
    }
    let mut st = highlight::State::start(lang, true);
    let mut spans = Vec::new();
    let mut caps = rules[0].re.create_captures();
    for (n, raw) in text.split('\n').enumerate() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        spans.clear();
        let before = st;
        st = highlight::spans_from(lang, st, line, true, &mut spans);
        // A line that starts inside a comment or string holds no definition at its start.
        if before.in_comment() || before.in_string() || line.trim().is_empty() {
            continue;
        }
        let mut code = line.as_bytes().to_vec();
        for &(s, e, k) in &spans {
            if matches!(k, Kind::Comment | Kind::Str) {
                for b in &mut code[s.min(line.len())..e.min(line.len())] {
                    *b = b' ';
                }
            }
        }
        for r in rules {
            r.re.captures(code.as_slice(), &mut caps);
            let Some(g) = caps.get_group(1) else { continue };
            let name = line[g.range()].trim();
            if NOT_NAMES.contains(&name) {
                continue;
            }
            // A C-style call or declaration is not a definition.
            if r.kind == SymKind::Function && matches!(lang, Lang::C | Lang::CSharp | Lang::Java | Lang::Dart) && r.prefix.is_empty() {
                let t = String::from_utf8_lossy(&code);
                let t = t.trim_end();
                let first = t.split_whitespace().next().unwrap_or("");
                if t.ends_with(';') || NOT_NAMES.contains(&first) || t.contains('=') && t.find('=') < t.find('(') {
                    continue;
                }
            }
            let indent: u32 = line.chars().take_while(|c| *c == ' ' || *c == '\t').map(|c| if c == '\t' { bigtext::text::rows::TAB_WIDTH as u32 } else { 1 }).sum();
            out.push(Symbol { name: format!("{}{name}", r.prefix), kind: r.kind, line: n as u64, depth: (indent / unit.max(1)).min(8) as u8 });
            break;
        }
    }
    out
}

/// Go to symbol's state.
#[derive(Default)]
pub struct SymbolsUi {
    pub doc: u64,
    pub version: u64,
    pub list: Option<Vec<Symbol>>,
    /// The file was longer than [`SCAN_MAX`].
    pub cut: bool,
    pub query: String,
    pub selected: usize,
    /// Shown (closed, the list is kept for the next Ctrl+R on the same version).
    pub open: bool,
    focus: bool,
    rx: Option<mpsc::Receiver<(Vec<Symbol>, bool)>>,
}

impl BigTextApp {
    /// Search → Go to symbol.
    pub(crate) fn open_symbols(&mut self) {
        let Some(d) = self.docs.get(self.active) else { return };
        let lang = d.effective_lang();
        if !supported(lang) || !d.is_ready() {
            self.notify(tr("Go to symbol lists the functions and classes of code files.").into(), None);
            return;
        }
        if let Some(s) = self.symbols.as_mut().filter(|s| s.doc == d.id && s.version == d.version && s.list.is_some()) {
            s.query.clear();
            s.selected = 0;
            s.focus = true;
            s.open = true;
            return;
        }
        let (snap, len) = (d.buf.snapshot(), d.buf.len());
        let unit = d.view.indent_unit.map_or(4, |u| u.1);
        let (tx, rx) = mpsc::channel();
        let waker = self.waker.clone();
        bigtext::jobs::spawn("symbols", move || {
            let n = len.min(SCAN_MAX);
            let list = snap.read(0..n).map(|b| scan(lang, &snap.codec.decode_lossy(&b), unit)).unwrap_or_default();
            let _ = tx.send((list, len > SCAN_MAX));
            waker();
        });
        self.symbols = Some(SymbolsUi { doc: d.id, version: d.version, focus: true, open: true, rx: Some(rx), ..Default::default() });
    }

    pub(super) fn symbols_window(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.symbols else { return };
        if let Some(rx) = &s.rx {
            if let Ok((list, cut)) = rx.try_recv() {
                s.list = Some(list);
                s.cut = cut;
                s.rx = None;
            }
        }
        if !s.open {
            return;
        }
        let (up, down, enter, esc) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        let list = s.list.clone().unwrap_or_default();
        let names: Vec<String> = list.iter().map(|x| x.name.clone()).collect();
        let found = |q: &str| -> Vec<usize> {
            if q.trim().is_empty() {
                (0..list.len()).collect()
            } else {
                super::quick_open::best(q, &names, SHOWN).into_iter().map(|(i, _)| i).collect()
            }
        };
        let mut shown = found(&s.query);
        s.selected = s.selected.min(shown.len().saturating_sub(1));
        if up {
            s.selected = s.selected.saturating_sub(1);
        }
        if down && s.selected + 1 < shown.len() {
            s.selected += 1;
        }
        let mut chosen = None;
        let reading = s.rx.is_some();
        egui::Window::new("symbols")
            .title_bar(false)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 60.0))
            .fixed_size(egui::vec2(520.0, 0.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let r = ui.add(egui::TextEdit::singleline(&mut s.query).hint_text(tr("Go to symbol")).desired_width(ui.available_width() - if reading { 24.0 } else { 0.0 }));
                    if s.focus {
                        r.request_focus();
                        s.focus = false;
                    }
                    if r.changed() {
                        s.selected = 0;
                    }
                    if reading {
                        ui.spinner().on_hover_text(tr("Reading…"));
                    }
                });
                ui.separator();
                let weak = ui.visuals().weak_text_color();
                let info = super::theme::info_text(ui);
                egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                    for (k, &i) in shown.iter().enumerate() {
                        let sym = &list[i];
                        let r = ui
                            .horizontal(|ui| {
                                ui.add_space(sym.depth as f32 * 12.0);
                                ui.colored_label(info, sym.kind.badge());
                                let b = egui::Button::selectable(k == s.selected, &sym.name).shortcut_text(egui::RichText::new(format!("{}", sym.line + 1)).color(weak));
                                ui.add(b.min_size(egui::vec2(ui.available_width(), 0.0)))
                            })
                            .inner;
                        if k == s.selected {
                            r.scroll_to_me(None);
                        }
                        if r.clicked() {
                            chosen = Some(i);
                        }
                    }
                    if shown.is_empty() && !reading {
                        ui.weak(if list.is_empty() { tr("No functions or classes were found.") } else { tr("No symbol matches.") });
                    }
                });
                if s.cut {
                    ui.weak(trf("Only the first {size} of the file are read.", &[("size", &bigtext::jobs::human_bytes(SCAN_MAX))]));
                }
            });
        // Enter takes the text typed in this frame too.
        if enter && chosen.is_none() {
            shown = found(&s.query);
            chosen = shown.get(s.selected.min(shown.len().saturating_sub(1))).copied();
        }
        let doc = s.doc;
        if let Some(i) = chosen {
            s.open = false;
            let line = list[i].line + 1;
            if let Some(d) = self.docs.iter_mut().find(|d| d.id == doc) {
                d.goto_line(line);
            }
        } else if esc {
            s.open = false;
        }
    }

    /// Go to symbol is open (it takes the keys).
    pub(crate) fn symbols_open(&self) -> bool {
        self.symbols.as_ref().is_some_and(|s| s.open)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(lang: Lang, text: &str) -> Vec<(String, SymKind, u64, u8)> {
        scan(lang, text, 4).into_iter().map(|s| (s.name, s.kind, s.line, s.depth)).collect()
    }

    #[test]
    fn python_and_javascript() {
        let py = "class User:\n    \"\"\"Docs.\n    def not_this(): ...\n    \"\"\"\n    def greet(self):\n        pass\n\nasync def main():\n    x = 'def nope():'\n";
        assert_eq!(names(Lang::Python, py), vec![("User".into(), SymKind::Class, 0, 0), ("greet".into(), SymKind::Function, 4, 1), ("main".into(), SymKind::Function, 7, 0)]);
        let js = "export async function load(url) {\n}\nconst add = (a, b) => a + b;\nclass View {\n  render() {\n    if (x) {\n    }\n  }\n}\n// function commented() {}\n";
        let got: Vec<String> = names(Lang::JavaScript, js).into_iter().map(|s| s.0).collect();
        assert_eq!(got, ["load", "add", "View", "render"]);
    }

    #[test]
    fn rust_go_c_family() {
        let rs = "pub struct A;\nimpl Display for A {\n    pub(crate) async fn fmt(&self) {}\n}\nmacro_rules! m { }\n";
        let got: Vec<(String, SymKind)> = names(Lang::Rust, rs).into_iter().map(|s| (s.0, s.1)).collect();
        assert_eq!(got, [("A".into(), SymKind::Type), ("impl Display for A".into(), SymKind::Class), ("fmt".into(), SymKind::Function), ("m".into(), SymKind::Macro)]);
        let go = "func (s *Server) Handle(w http.ResponseWriter) error {\n}\ntype Server struct {}\n";
        assert_eq!(names(Lang::Go, go).into_iter().map(|s| s.0).collect::<Vec<_>>(), ["Handle", "Server"]);
        let java = "public class Demo {\n    @Override\n    public String toString() {\n        return format(x);\n    }\n    int n = count(1);\n    void run();\n    } else if (x) {\n}\n";
        assert_eq!(names(Lang::Java, java).into_iter().map(|s| s.0).collect::<Vec<_>>(), ["Demo", "toString"]);
        let c = "#define MAX 10\nstatic int count(const char *s) {\n    printf(\"x\");\n}\nstruct point {\n";
        assert_eq!(names(Lang::C, c).into_iter().map(|s| s.0).collect::<Vec<_>>(), ["MAX", "count", "point"]);
        assert!(!supported(Lang::Json) && supported(Lang::Shell));
        let kt = "data class User(val id: Int)\nobject Registry {\n    suspend fun <T> load(x: T): T = x\n    fun String.shout() = uppercase()\n}\n";
        assert_eq!(names(Lang::Kotlin, kt).into_iter().map(|s| s.0).collect::<Vec<_>>(), ["User", "Registry", "load", "shout"]);
        let sw = "public struct Point {\n    mutating func move() {}\n}\nextension Point: Codable {}\n";
        assert_eq!(names(Lang::Swift, sw).into_iter().map(|s| s.0).collect::<Vec<_>>(), ["Point", "move", "Point"]);
        let rb = "module Shop\n  class Cart < Base\n    def empty?\n    end\n    def self.build\n    end\n  end\nend\n";
        assert_eq!(names(Lang::Ruby, rb).into_iter().map(|s| (s.0, s.3)).collect::<Vec<_>>(), [("Shop".into(), 0), ("Cart".into(), 0), ("empty?".into(), 1), ("build".into(), 1)]);
        let lua = "local function helper(x)\nend\nfunction M.run()\nend\nM.stop = function() end\n";
        assert_eq!(names(Lang::Lua, lua).into_iter().map(|s| s.0).collect::<Vec<_>>(), ["helper", "M.run", "M.stop"]);
        let ps = "Function Get-Thing {\n}\nclass Point {\n}\n# function Not-This\n";
        assert_eq!(names(Lang::PowerShell, ps).into_iter().map(|s| s.0).collect::<Vec<_>>(), ["Get-Thing", "Point"]);
        let dart = "abstract class Shape {}\nFuture<void> main() async {\n  print(1);\n}\nmixin Fly {}\n";
        assert_eq!(names(Lang::Dart, dart).into_iter().map(|s| s.0).collect::<Vec<_>>(), ["Shape", "main", "Fly"]);
    }
}
