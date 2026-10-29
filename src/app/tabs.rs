//! The tab bar and tab switching: pinned tabs stay first and have no close button; drag a tab
//! to move it; right-click for pin, close others, close to the right and copy path; Ctrl+Tab
//! shows the tabs in most-recently-used order (hold Ctrl, press Tab to step, release to switch).

use egui::{Key, Modifiers, RichText};

use bigtext::file;

use super::i18n::{tr, trf};
use super::BigTextApp;

pub struct TabSwitcher {
    /// Document ids, most recently used first.
    pub order: Vec<u64>,
    pub selected: usize,
}

impl BigTextApp {
    /// Keeps `mru` in step with the active tab and the open documents.
    pub(super) fn track_mru(&mut self) {
        let ids: Vec<u64> = self.docs.iter().map(|d| d.id).collect();
        self.mru.retain(|id| ids.contains(id));
        if let Some(id) = self.docs.get(self.active).map(|d| d.id) {
            if self.mru.first() != Some(&id) {
                self.mru.retain(|x| *x != id);
                self.mru.insert(0, id);
            }
        }
        for id in ids {
            if !self.mru.contains(&id) {
                self.mru.push(id);
            }
        }
    }

    fn activate(&mut self, id: u64) {
        if let Some(i) = self.docs.iter().position(|d| d.id == id) {
            self.active = i;
        }
    }

    /// Ctrl+Tab / Ctrl+Shift+Tab: opens the switcher, or steps through it.
    pub(crate) fn switch_tab(&mut self, forward: bool) {
        if self.docs.len() < 2 {
            return;
        }
        self.track_mru();
        let n = self.mru.len();
        match &mut self.switcher {
            Some(s) => s.selected = if forward { (s.selected + 1) % n } else { (s.selected + n - 1) % n },
            None => self.switcher = Some(TabSwitcher { order: self.mru.clone(), selected: if forward { 1 } else { n - 1 } }),
        }
    }

    /// Keys while the switcher is open (before the text view sees them).
    pub(super) fn switcher_keys(&mut self, ctx: &egui::Context) {
        if self.switcher.is_none() {
            return;
        }
        let (next, prev, up, down, enter, esc, ctrl) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::CTRL, Key::Tab),
                i.consume_key(Modifiers::CTRL | Modifiers::SHIFT, Key::Tab),
                i.consume_key(Modifiers::NONE, Key::ArrowUp) || i.consume_key(Modifiers::CTRL, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown) || i.consume_key(Modifiers::CTRL, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape) || i.consume_key(Modifiers::CTRL, Key::Escape),
                i.modifiers.ctrl,
            )
        });
        if prev || up {
            self.switch_tab(false);
        } else if next || down {
            self.switch_tab(true);
        }
        if esc {
            self.switcher = None;
        } else if enter || !ctrl {
            // Releasing Ctrl switches, as in most editors.
            if let Some(s) = self.switcher.take() {
                if let Some(&id) = s.order.get(s.selected) {
                    self.activate(id);
                }
            }
        }
    }

    pub(super) fn switcher_window(&mut self, ctx: &egui::Context) {
        let Some(s) = &self.switcher else { return };
        let mut pick = None;
        egui::Area::new(egui::Id::new("tab-switcher")).anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 80.0)).order(egui::Order::Foreground).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(360.0);
                ui.weak(tr("Hold Ctrl, press Tab to step, release to switch"));
                ui.separator();
                for (k, id) in s.order.iter().enumerate() {
                    let Some(d) = self.docs.iter().find(|d| d.id == *id) else { continue };
                    let mut label = d.title.clone();
                    if d.is_modified() {
                        label.push_str(" *");
                    }
                    let r = ui.selectable_label(k == s.selected, RichText::new(label).strong());
                    if r.clicked() {
                        pick = Some(*id);
                    }
                    if let Some(p) = d.path.as_deref() {
                        ui.weak(file::display_path(p));
                    }
                }
            });
        });
        if let Some(id) = pick {
            self.switcher = None;
            self.activate(id);
        }
    }

    /// Pins or unpins a tab; pinned tabs stay at the front.
    pub(crate) fn toggle_pin(&mut self, id: u64) {
        let active = self.docs.get(self.active).map(|d| d.id);
        if let Some(d) = self.docs.iter_mut().find(|d| d.id == id) {
            d.pinned = !d.pinned;
        }
        self.docs.sort_by_key(|d| !d.pinned);
        if let Some(a) = active {
            self.activate(a);
        }
        self.persist_state();
    }

    /// Moves the tab at `from` to `to`, within its group (pinned or not).
    pub(crate) fn move_tab(&mut self, from: usize, to: usize) {
        if from >= self.docs.len() || to >= self.docs.len() || from == to {
            return;
        }
        let pinned = self.docs[from].pinned;
        let (lo, hi) = {
            let n_pinned = self.docs.iter().filter(|d| d.pinned).count();
            if pinned { (0, n_pinned.saturating_sub(1)) } else { (n_pinned, self.docs.len() - 1) }
        };
        let to = to.clamp(lo, hi);
        let active = self.docs.get(self.active).map(|d| d.id);
        let d = self.docs.remove(from);
        self.docs.insert(to, d);
        if let Some(a) = active {
            self.activate(a);
        }
    }

    /// Closes the given tabs; ones with unsaved changes stay open (and are counted).
    fn close_many(&mut self, ids: Vec<u64>) {
        let mut kept = 0;
        for id in ids {
            match self.docs.iter().find(|d| d.id == id) {
                Some(d) if d.is_modified() => kept += 1,
                Some(_) => self.close_tab(id, false),
                None => {}
            }
        }
        if kept > 0 {
            self.notify(trf("{n} tabs with unsaved changes were kept open.", &[("n", &kept.to_string())]), None);
        }
    }

    /// Closes every unpinned tab but `keep`.
    pub(crate) fn close_others(&mut self, keep: u64) {
        let ids = self.docs.iter().filter(|d| d.id != keep && !d.pinned).map(|d| d.id).collect();
        self.close_many(ids);
        self.activate(keep);
    }

    /// Closes the unpinned tabs to the right of `of`.
    pub(crate) fn close_to_right(&mut self, of: u64) {
        let Some(i) = self.docs.iter().position(|d| d.id == of) else { return };
        let ids = self.docs[i + 1..].iter().filter(|d| !d.pinned).map(|d| d.id).collect();
        self.close_many(ids);
    }

    pub(super) fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let mut close = None;
        let mut activate = None;
        let mut moved = None;
        let mut menu: Option<(u64, &'static str)> = None;
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.horizontal(|ui| {
                for (i, d) in self.docs.iter().enumerate() {
                    let mut label = if d.pinned { format!("📌 {}", d.title) } else { d.title.clone() };
                    if d.is_modified() {
                        label.push_str(" *");
                    }
                    let r = ui.add(egui::Button::selectable(i == self.active, label).sense(egui::Sense::click_and_drag()));
                    r.dnd_set_drag_payload(i);
                    if r.dnd_hover_payload::<usize>().is_some_and(|from| *from != i) {
                        let x = if r.dnd_hover_payload::<usize>().is_some_and(|from| *from < i) { r.rect.right() } else { r.rect.left() };
                        ui.painter().vline(x, r.rect.y_range(), egui::Stroke::new(2.0, ui.visuals().selection.stroke.color));
                    }
                    if let Some(from) = r.dnd_release_payload::<usize>() {
                        moved = Some((*from, i));
                    }
                    if r.clicked() {
                        activate = Some(i);
                    }
                    if r.middle_clicked() && !d.pinned {
                        close = Some(d.id);
                    }
                    let r = r.on_hover_text(d.path.as_deref().map(file::display_path).unwrap_or_default());
                    r.context_menu(|ui| {
                        let id = d.id;
                        let mut item = |ui: &mut egui::Ui, label: &'static str| {
                            if ui.button(tr(label)).clicked() {
                                menu = Some((id, label));
                                ui.close();
                            }
                        };
                        item(ui, if d.pinned { "Unpin tab" } else { "Pin tab" });
                        ui.separator();
                        item(ui, "Close");
                        item(ui, "Close other tabs");
                        item(ui, "Close tabs to the right");
                        if d.path.is_some() {
                            ui.separator();
                            item(ui, "Copy path");
                            item(ui, "Show in folder");
                            item(ui, "Open terminal here");
                            item(ui, "Open with the default app");
                        }
                    });
                    if !d.pinned && ui.small_button("×").clicked() {
                        close = Some(d.id);
                    }
                    ui.separator();
                }
            });
        });
        if let Some(i) = activate {
            self.active = i;
        }
        if let Some((from, to)) = moved {
            self.move_tab(from, to);
        }
        if let Some(id) = close {
            self.close_tab(id, false);
        }
        if let Some((id, what)) = menu {
            match what {
                "Pin tab" | "Unpin tab" => self.toggle_pin(id),
                "Close" => self.close_tab(id, false),
                "Close other tabs" => self.close_others(id),
                "Close tabs to the right" => self.close_to_right(id),
                "Show in folder" => self.shell_doc(id, super::shell::ShellAction::Reveal),
                "Open terminal here" => self.shell_doc(id, super::shell::ShellAction::Terminal),
                "Open with the default app" => self.shell_doc(id, super::shell::ShellAction::Default),
                "Copy path" => {
                    if let Some(p) = self.docs.iter().find(|d| d.id == id).and_then(|d| d.unpacked_from.as_deref().or(d.path.as_deref())) {
                        ui.ctx().copy_text(file::display_path(p));
                    }
                }
                _ => {}
            }
        }
    }
}
