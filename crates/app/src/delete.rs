//! Deleting files and folders from the app: confirmation dialog, background worker, tree update.

use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use eframe::egui::{self, Color32, Id, Key, RichText};
use spacetree_core::stats::human_size;
use spacetree_core::{NodeId, ROOT, Tree};

use crate::{App, ToastKind, theme, winint};

/// Folders under the volume root whose contents belong to Windows or installed programs.
const SYSTEM_DIRS: &[&str] = &["windows", "program files", "program files (x86)", "programdata"];

pub struct Pending {
    pub ids: Vec<NodeId>,
    pub permanent: bool,
    /// Empty the recycle bin instead of deleting nodes (the ids are the `$Recycle.Bin` folders).
    pub empty_bin: bool,
    pub size: u64,
    pub system: bool,
}

pub struct Running {
    rx: Receiver<Outcome>,
    done: Arc<AtomicUsize>,
    total: usize,
    current: Arc<Mutex<String>>,
    started: Instant,
}

struct Outcome {
    /// Nodes that are gone from disk.
    removed: Vec<NodeId>,
    failed: Vec<(String, String)>,
}

#[derive(Default)]
pub struct DeleteState {
    pub pending: Option<Pending>,
    pub running: Option<Running>,
    /// Finished results waiting until the tree can be modified (a CSV export may still read it).
    waiting: Option<Outcome>,
}

impl DeleteState {
    pub fn idle(&self) -> bool {
        self.pending.is_none() && self.running.is_none()
    }
}

/// `C:\Windows\...` and the like.
pub fn is_system(tree: &Tree, id: NodeId) -> bool {
    let anc = tree.ancestors(id);
    // ancestors()[0] is the scan root; system folders are only recognised under a volume root
    let root = tree.name(ROOT);
    let volume_root = root.len() <= 3 && root.as_bytes().get(1) == Some(&b':');
    if volume_root && let Some(&first) = anc.get(1) {
        return SYSTEM_DIRS.iter().any(|d| tree.name(first).eq_ignore_ascii_case(d));
    }
    let p = tree.path(id).to_lowercase().replace('/', "\\");
    SYSTEM_DIRS
        .iter()
        .any(|d| p.contains(&format!(":\\{d}\\")) || p.ends_with(&format!(":\\{d}")))
}

/// Drops ids whose ancestor is also in the list (deleting the folder deletes them anyway).
fn top_level(tree: &Tree, ids: &[NodeId]) -> Vec<NodeId> {
    let set: std::collections::HashSet<NodeId> = ids.iter().copied().collect();
    let mut out: Vec<NodeId> = ids
        .iter()
        .copied()
        .filter(|&id| id != ROOT && !tree.is_removed(id))
        .filter(|&id| !tree.ancestors(id).iter().rev().skip(1).any(|a| set.contains(a)))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

impl App {
    pub fn request_delete(&mut self, ids: Vec<NodeId>, permanent: bool) {
        let Some(data) = self.data.clone() else { return };
        let tree = &data.out.tree;
        let ids = top_level(tree, &ids);
        if ids.is_empty() {
            if self.selected == Some(ROOT) {
                self.toast("Корневую папку сканирования удалить нельзя", ToastKind::Error);
            }
            return;
        }
        let pending = Pending {
            size: ids.iter().map(|&i| tree.node(i).size).sum(),
            system: ids.iter().any(|&i| is_system(tree, i)),
            ids,
            permanent,
            empty_bin: false,
        };
        if !pending.permanent && !pending.system && !self.settings.confirm_delete {
            self.start_delete(pending);
        } else {
            self.delete.pending = Some(pending);
        }
    }

    pub fn request_empty_bin(&mut self, bins: Vec<NodeId>) {
        let Some(data) = self.data.clone() else { return };
        let size = bins.iter().map(|&i| data.out.tree.node(i).size).sum();
        self.delete.pending = Some(Pending {
            ids: bins,
            permanent: true,
            empty_bin: true,
            size,
            system: false,
        });
    }

    fn start_delete(&mut self, p: Pending) {
        if crate::settings::is_demo() {
            self.toast("В демо-режиме ничего не удаляется", ToastKind::Info);
            return;
        }
        let Some(data) = self.data.clone() else { return };
        let tree = &data.out.tree;
        // (node, path, is the recycle-bin folder whose children go away)
        let jobs: Vec<(NodeId, String)> = p.ids.iter().map(|&i| (i, tree.path(i))).collect();
        let bin_children: Vec<NodeId> = if p.empty_bin {
            p.ids.iter().flat_map(|&b| tree.children(b).to_vec()).collect()
        } else {
            Vec::new()
        };
        let (tx, rx) = channel();
        let done = Arc::new(AtomicUsize::new(0));
        let current = Arc::new(Mutex::new(String::new()));
        let total = jobs.len();
        let (d2, c2) = (done.clone(), current.clone());
        let (permanent, empty_bin) = (p.permanent, p.empty_bin);
        std::thread::spawn(move || {
            let mut out = Outcome {
                removed: Vec::new(),
                failed: Vec::new(),
            };
            if empty_bin {
                match winint::empty_recycle_bin() {
                    Ok(()) => out.removed = bin_children,
                    Err(e) => out.failed.push(("Корзина".into(), e)),
                }
            } else {
                for (id, path) in jobs {
                    *c2.lock().unwrap() = path.clone();
                    let p = std::path::Path::new(&path);
                    let res = if permanent {
                        if p.is_dir() {
                            std::fs::remove_dir_all(p).map_err(|e| e.to_string())
                        } else {
                            std::fs::remove_file(p).map_err(|e| e.to_string())
                        }
                    } else {
                        winint::recycle(&path)
                    };
                    // What matters is whether it is gone: a folder may be deleted only partially.
                    let gone = std::fs::symlink_metadata(p).is_err();
                    if gone {
                        out.removed.push(id);
                    } else {
                        let why = res
                            .err()
                            .unwrap_or_else(|| "удалено не полностью (файлы заняты?)".into());
                        out.failed.push((path, why));
                    }
                    d2.fetch_add(1, Relaxed);
                }
            }
            let _ = tx.send(out);
        });
        self.delete.running = Some(Running {
            rx,
            done,
            total,
            current,
            started: Instant::now(),
        });
    }

    pub fn poll_delete(&mut self) {
        if let Some(r) = &self.delete.running {
            match r.rx.try_recv() {
                Ok(o) => {
                    crate::diag::info(format!(
                        "delete finished: {} removed, {} failed",
                        o.removed.len(),
                        o.failed.len()
                    ));
                    for (path, why) in &o.failed {
                        crate::diag::warn(format!("cannot delete {path}: {why}"));
                    }
                    self.delete.running = None;
                    self.delete.waiting = Some(o);
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.delete.running = None;
                    self.toast("Удаление прервалось", ToastKind::Error);
                }
            }
        }
        let Some(outcome) = self.delete.waiting.take() else {
            return;
        };
        let Some(data) = self.data.as_mut().and_then(std::sync::Arc::get_mut) else {
            self.delete.waiting = Some(outcome); // a background reader still holds the tree; retry next frame
            return;
        };
        let before = data.out.tree.root_stats().0;
        for &id in &outcome.removed {
            data.out.tree.remove(id);
        }
        let freed = before - data.out.tree.root_stats().0;
        if !outcome.removed.is_empty() {
            data.reanalyze();
        }
        self.tree_changed();
        if !outcome.removed.is_empty() {
            self.toast(
                format!(
                    "Удалено объектов: {} · освобождено {}",
                    outcome.removed.len(),
                    human_size(freed)
                ),
                ToastKind::Success,
            );
        }
        if let Some((path, why)) = outcome.failed.first() {
            let more = if outcome.failed.len() > 1 {
                format!(" (и ещё {})", outcome.failed.len() - 1)
            } else {
                String::new()
            };
            self.toast(format!("Не удалось удалить {path}: {why}{more}"), ToastKind::Error);
        }
    }

    /// Confirmation modal and progress modal.
    pub fn delete_dialogs(&mut self, ctx: &egui::Context) {
        if let Some(r) = &self.delete.running {
            let done = r.done.load(Relaxed);
            let current = r.current.lock().unwrap().clone();
            let slow = r.started.elapsed().as_millis() > 300; // don't flash a dialog for quick deletions
            let total = r.total;
            if slow {
                egui::Modal::new(Id::new("deleting")).show(ctx, |ui| {
                    ui.set_width(420.0);
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new());
                        ui.label(RichText::new("Удаление…").strong());
                    });
                    ui.add(egui::ProgressBar::new(done as f32 / total.max(1) as f32).show_percentage());
                    ui.label(RichText::new(current).weak().small());
                });
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
            return;
        }
        let Some(p) = &mut self.delete.pending else { return };
        let Some(data) = self.data.clone() else { return };
        let tree = &data.out.tree;
        let mut decision = None;
        egui::Modal::new(Id::new("confirm_delete")).show(ctx, |ui| {
            ui.set_width(480.0);
            let title = if p.empty_bin {
                "Очистить корзину?".to_owned()
            } else if p.ids.len() == 1 {
                format!("Удалить «{}»?", tree.name(p.ids[0]))
            } else {
                format!("Удалить {} объектов?", p.ids.len())
            };
            ui.label(RichText::new(title).size(17.0).strong());
            ui.label(format!("Будет освобождено около {}", human_size(p.size)));
            ui.add_space(4.0);
            if !p.empty_bin {
                egui::ScrollArea::vertical().max_height(150.0).show(ui, |ui| {
                    for &id in p.ids.iter().take(50) {
                        ui.label(
                            RichText::new(format!("{}  ·  {}", tree.path(id), human_size(tree.node(id).size)))
                                .weak()
                                .small(),
                        );
                    }
                    if p.ids.len() > 50 {
                        ui.label(RichText::new(format!("…и ещё {}", p.ids.len() - 50)).weak().small());
                    }
                });
                ui.add_space(4.0);
                ui.checkbox(&mut p.permanent, "Удалить безвозвратно, минуя корзину");
            }
            if p.system {
                ui.label(
                    RichText::new(
                        "⚠ Среди выбранного есть файлы Windows или установленных программ. Их удаление может \
                         сломать систему или приложения.",
                    )
                    .color(theme::REVIEW),
                );
            }
            if p.permanent {
                ui.label(RichText::new("Восстановить файлы после этого будет нельзя.").color(theme::REVIEW));
            } else if !p.empty_bin {
                ui.label(RichText::new("Файлы попадут в корзину — их можно будет восстановить.").weak());
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let label = if p.empty_bin {
                    "Очистить"
                } else if p.permanent {
                    "Удалить навсегда"
                } else {
                    "В корзину"
                };
                let danger = ui.add(
                    egui::Button::new(RichText::new(label).color(Color32::WHITE).strong()).fill(if p.permanent {
                        theme::REVIEW
                    } else {
                        theme::ACCENT
                    }),
                );
                if danger.clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                    decision = Some(true);
                }
                if ui.button("Отмена").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                    decision = Some(false);
                }
            });
        });
        match decision {
            Some(true) => {
                let p = self.delete.pending.take().unwrap();
                self.start_delete(p);
            }
            Some(false) => self.delete.pending = None,
            None => {}
        }
    }
}
