//! Back and forward (Alt+Left / Alt+Right): the places the caret jumped away from — with Go to,
//! F3, F8, a bookmark, a click in a result list — and the tabs left, across tabs.

use super::BigTextApp;

/// Places kept.
const NAV_MAX: usize = 100;

/// A place: document, caret and top of the view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NavEntry {
    pub doc: u64,
    pub caret: u64,
    pub top: u64,
}

#[derive(Default)]
pub struct NavHistory {
    pub back: Vec<NavEntry>,
    pub forward: Vec<NavEntry>,
    /// The place and jump count at the start of the last frame.
    last: Option<(NavEntry, u64)>,
}

impl NavHistory {
    fn push_back(&mut self, e: NavEntry) {
        if self.back.last() != Some(&e) {
            self.back.push(e);
            if self.back.len() > NAV_MAX {
                self.back.remove(0);
            }
        }
    }
}

impl BigTextApp {
    fn here(&self) -> Option<(NavEntry, u64)> {
        let d = self.docs.get(self.active)?;
        Some((NavEntry { doc: d.id, caret: d.view.caret, top: d.view.top }, d.view.jumps))
    }

    /// Notes, once a frame, whether the caret jumped or another tab came up since the last frame;
    /// if so, the place before is kept for Back.
    pub(super) fn track_jumps(&mut self) {
        let Some(now) = self.here() else { return };
        if let Some((before, jumps)) = self.nav.last {
            let moved = if before.doc == now.0.doc { now.1 != jumps && before.caret != now.0.caret } else { true };
            if moved && self.docs.iter().any(|d| d.id == before.doc) {
                self.nav.push_back(before);
                self.nav.forward.clear();
            }
        }
        self.nav.last = Some(now);
    }

    /// Alt+Left: back to the place before the last jump.
    pub(crate) fn nav_back(&mut self) {
        self.nav_step(true);
    }

    /// Alt+Right: forward again.
    pub(crate) fn nav_forward(&mut self) {
        self.nav_step(false);
    }

    fn nav_step(&mut self, back: bool) {
        let Some((now, _)) = self.here() else { return };
        loop {
            let next = if back { self.nav.back.pop() } else { self.nav.forward.pop() };
            let Some(e) = next else { return };
            let Some(i) = self.docs.iter().position(|d| d.id == e.doc) else { continue };
            if e == now {
                continue;
            }
            if back {
                self.nav.forward.push(now);
            } else {
                self.nav.back.push(now);
            }
            self.active = i;
            let d = &mut self.docs[i];
            let len = d.buf.len();
            d.set_caret(e.caret.min(len), false);
            d.view.top = e.top.min(len);
            d.view.scroll_to_caret = false;
            self.nav.last = self.here();
            return;
        }
    }
}
