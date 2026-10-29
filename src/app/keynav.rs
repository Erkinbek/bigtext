//! Moving around without the mouse: Ctrl+1…Ctrl+8 to a tab (Ctrl+9 the last one),
//! Ctrl+PageDown / Ctrl+PageUp to the next / previous tab in the bar, and F4 / Shift+F4 to the
//! next / previous entry of the list under the text (Find All, the filter, Find in files).

use super::i18n::trf;
use super::BigTextApp;

impl BigTextApp {
    /// Ctrl+N: tab `n` (1-based); 9 is the last tab.
    pub(crate) fn goto_tab(&mut self, n: u8) {
        let count = self.docs.len();
        if count == 0 {
            return;
        }
        let i = if n >= 9 { count - 1 } else { n as usize - 1 };
        if i < count {
            self.active = i;
        }
    }

    /// The next (or previous) tab in the order of the tab bar, around the ends.
    pub(crate) fn step_tab_bar(&mut self, forward: bool) {
        let n = self.docs.len();
        if n > 1 {
            self.active = if forward { (self.active + 1) % n } else { (self.active + n - 1) % n };
        }
    }

    /// F4 / Shift+F4: the next or previous entry of the list under the text, around the ends.
    pub(crate) fn step_result(&mut self, forward: bool) {
        let show_results = self.search.show_results;
        let show_filter = self.search.show_filter;
        let Some(d) = self.docs.get_mut(self.active) else { return };
        let caret = d.view.caret;
        // Find All.
        if let Some(r) = d.results.as_ref().filter(|r| show_results && r.version == d.version) {
            let g = r.data.lock().unwrap();
            let n = g.matches.len();
            if n > 0 {
                let i = if forward {
                    let i = g.matches.partition_point(|&(s, _)| s <= caret);
                    if i < n { i } else { 0 }
                } else {
                    let i = g.matches.partition_point(|&(s, _)| s < d.view.anchor.min(caret));
                    if i > 0 { i - 1 } else { n - 1 }
                };
                let (s, e) = g.matches[i];
                drop(g);
                d.select(s, e);
                d.view.center();
                let msg = trf("Match {i} of {n}", &[("i", &(i + 1).to_string()), ("n", &n.to_string())]);
                self.notify(msg, None);
                return;
            }
        }
        // The filter's lines with a match.
        if let Some(r) = d.filter.as_ref().filter(|r| show_filter && r.version == d.version) {
            let here = d.line_bounds(caret, 64 * 1024).map(|b| b.0).unwrap_or(caret);
            let g = r.data.lock().unwrap();
            let hits: Vec<usize> = (0..g.lines.len()).filter(|&i| !g.is_context(i)).collect();
            if !hits.is_empty() {
                let pos = if forward {
                    hits.iter().position(|&i| g.lines[i] > here).unwrap_or(0)
                } else {
                    hits.iter().rposition(|&i| g.lines[i] < here).unwrap_or(hits.len() - 1)
                };
                let ls = g.lines[hits[pos]];
                drop(g);
                d.set_caret(ls.max(d.min_caret()), false);
                d.view.center();
                return;
            }
        }
        // Find in files.
        let Some(r) = &self.files.results else { return };
        let n = r.data.lock().unwrap().hits.len();
        if n == 0 {
            return;
        }
        let i = match self.files.current {
            Some(c) if forward => (c + 1) % n,
            Some(c) => (c + n - 1) % n,
            None if forward => 0,
            None => n - 1,
        };
        self.open_files_hit(i);
    }
}
