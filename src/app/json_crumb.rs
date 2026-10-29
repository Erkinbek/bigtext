//! The JSON path at the caret in the status bar ($ › items › [1234] › city): each part jumps to
//! that parent, a right-click copies the path. Deep in big files the path is found from the
//! nearest of the checkpoints one background pass records every 8 MB; JSON Lines are read from
//! the caret's line.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bigtext::jobs::{self, CancelToken, Progress};
use bigtext::json::{self, Checkpoint, Step};

use super::{BigTextApp, Msg};

/// Checkpoints are recorded this far apart, and smaller documents need none.
pub const EVERY: u64 = 8 << 20;
/// The path is looked up once the caret has rested this long.
const DEBOUNCE: Duration = Duration::from_millis(150);

pub struct JsonCrumb {
    /// The path, and the version and caret it is for.
    pub steps: Vec<(Step, u64)>,
    pub version: u64,
    pub caret: u64,
    pub error: bool,
    /// The (version, caret) wanted, and since when.
    want: (u64, u64),
    changed_at: Instant,
    /// A lookup on its way.
    busy: Option<CancelToken>,
    /// Checkpoints of a version, and a pass recording them.
    index: Option<(u64, Arc<Vec<Checkpoint>>)>,
    pub building: Option<(u64, CancelToken, Progress)>,
}

impl JsonCrumb {
    fn new() -> JsonCrumb {
        JsonCrumb { steps: Vec::new(), version: u64::MAX, caret: 0, error: false, want: (u64::MAX, 0), changed_at: Instant::now(), busy: None, index: None, building: None }
    }

    /// The path as text, `$.items[3].city`.
    pub fn path(&self) -> String {
        json::format_path(&self.steps.iter().map(|x| x.0.clone()).collect::<Vec<_>>())
    }
}

impl Drop for JsonCrumb {
    fn drop(&mut self) {
        if let Some(c) = &self.busy {
            c.cancel();
        }
        if let Some((_, c, _)) = &self.building {
            c.cancel();
        }
    }
}

pub enum CrumbMsg {
    Path { doc: u64, version: u64, caret: u64, result: Result<Vec<(Step, u64)>, String> },
    Index { doc: u64, version: u64, result: Result<Vec<Checkpoint>, String> },
}

/// One JSON value per line.
fn json_lines(d: &super::document::Document) -> bool {
    let ext = d.path.as_deref().and_then(|p| p.extension()).map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    matches!(ext.as_str(), "jsonl" | "ndjson")
}

impl BigTextApp {
    /// Looks up the path at the caret when it has moved (or the text changed) and rested.
    pub(super) fn json_crumb_tick(&mut self, ctx: &egui::Context) {
        if !self.settings.json_crumb {
            return;
        }
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        let Some(d) = self.docs.get_mut(self.active) else { return };
        if !d.is_ready() || d.hex || d.table || d.buf.codec.unit() != 1 || d.effective_lang() != crate::view::highlight::Lang::Json {
            return;
        }
        let jsonl = json_lines(d);
        let want = (d.version, d.view.caret);
        let c = d.json_crumb.get_or_insert_with(JsonCrumb::new);
        if c.want != want {
            c.want = want;
            c.changed_at = Instant::now();
        }
        if (c.version, c.caret) == want || c.busy.is_some() {
            return;
        }
        let wait = DEBOUNCE.saturating_sub(c.changed_at.elapsed());
        if !wait.is_zero() {
            ctx.request_repaint_after(wait);
            return;
        }
        // Where to start reading: the caret's line (JSON Lines), the start, or a checkpoint.
        let big = !jsonl && d.buf.len() > EVERY;
        let line = if jsonl { d.line_bounds(want.1, 64 << 20).map(|b| b.0) } else { None };
        let (buf, id, bom) = (d.buf.snapshot(), d.id, d.buf.codec.bom_len as u64);
        let Some(c) = d.json_crumb.as_mut() else { return };
        let start = match &c.index {
            _ if jsonl => match line {
                Some(pos) => Checkpoint { pos, stack: Vec::new() },
                None => return,
            },
            _ if !big => Checkpoint { pos: bom, stack: Vec::new() },
            Some((v, points)) if *v == want.0 => json::checkpoint_before(points, want.1).cloned().unwrap_or(Checkpoint { pos: bom, stack: Vec::new() }),
            _ => {
                // Checkpoints of this version first (the old path stays, greyed).
                if c.building.as_ref().is_none_or(|b| b.0 != want.0) {
                    if let Some((_, cancel, _)) = c.building.take() {
                        cancel.cancel();
                    }
                    let (cancel, progress) = (CancelToken::new(), Progress::default());
                    c.building = Some((want.0, cancel.clone(), progress.clone()));
                    jobs::spawn("json-checkpoints", move || {
                        let result = json::checkpoints(&buf, EVERY, &cancel, Some(&progress));
                        if !cancel.is_cancelled() {
                            let _ = tx.send(Msg::JsonCrumb(Box::new(CrumbMsg::Index { doc: id, version: want.0, result })));
                            waker();
                        }
                    });
                }
                return;
            }
        };
        let cancel = CancelToken::new();
        c.busy = Some(cancel.clone());
        jobs::spawn("json-path", move || {
            let result = json::path_at_from(&buf, &start, want.1, &cancel);
            let _ = tx.send(Msg::JsonCrumb(Box::new(CrumbMsg::Path { doc: id, version: want.0, caret: want.1, result })));
            waker();
        });
    }

    pub(super) fn on_json_crumb(&mut self, m: CrumbMsg) {
        match m {
            CrumbMsg::Path { doc, version, caret, result } => {
                let Some(c) = self.docs.iter_mut().find(|d| d.id == doc).and_then(|d| d.json_crumb.as_mut()) else { return };
                c.busy = None;
                match result {
                    Ok(steps) => {
                        c.steps = steps;
                        c.error = false;
                    }
                    Err(_) => c.error = true,
                }
                (c.version, c.caret) = (version, caret);
            }
            CrumbMsg::Index { doc, version, result } => {
                let Some(c) = self.docs.iter_mut().find(|d| d.id == doc).and_then(|d| d.json_crumb.as_mut()) else { return };
                if c.building.as_ref().is_some_and(|b| b.0 == version) {
                    c.building = None;
                }
                if let Ok(points) = result {
                    c.index = Some((version, Arc::new(points)));
                }
            }
        }
    }
}
