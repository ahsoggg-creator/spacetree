//! Deleting files and folders from the app: confirmation dialog, background worker, tree update.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
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
    /// Replace the duplicates by hard links to the copy that is kept instead of deleting them.
    pub link: bool,
    pub size: u64,
    pub system: bool,
}

pub struct Running {
    rx: Receiver<Outcome>,
    /// Progress in bytes (plus a fixed cost per file, so piles of empty files still move the bar).
    done: Arc<AtomicU64>,
    total: u64,
    current: Arc<Mutex<String>>,
    started: Instant,
}

struct Outcome {
    /// Nodes that are gone from disk.
    removed: Vec<NodeId>,
    /// Duplicates that now share the data of the kept copy.
    linked: Vec<NodeId>,
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

/// Progress weight of a subtree: its bytes plus a flat cost per file.
fn weight(size: u64, files: u32) -> u64 {
    size + u64::from(files.max(1)) * FILE_COST
}

const FILE_COST: u64 = 4096;

fn remove_file_progress(p: &Path, done: &AtomicU64) -> Result<(), String> {
    let size = std::fs::symlink_metadata(p).map(|m| m.len()).unwrap_or(0);
    let r = std::fs::remove_file(p).map_err(|e| e.to_string());
    if r.is_ok() {
        done.fetch_add(size + FILE_COST, Relaxed);
    }
    r
}

/// Depth-first removal that keeps going after an error and reports the first one.
fn remove_dir_progress(dir: &Path, done: &AtomicU64, current: &Mutex<String>) -> Result<(), String> {
    let mut first_err = None;
    match std::fs::read_dir(dir) {
        Ok(rd) => {
            for entry in rd.flatten() {
                let path = entry.path();
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                let r = if is_dir {
                    remove_dir_progress(&path, done, current)
                } else {
                    *current.lock().unwrap() = path.to_string_lossy().into_owned();
                    remove_file_progress(&path, done)
                };
                if let Err(e) = r {
                    first_err.get_or_insert(e);
                }
            }
        }
        Err(e) => first_err = Some(e.to_string()),
    }
    match std::fs::remove_dir(dir) {
        Ok(()) => first_err.map_or(Ok(()), Err),
        Err(e) => Err(first_err.unwrap_or_else(|| e.to_string())),
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
            link: false,
        };
        if !pending.permanent && !pending.system && !self.settings.confirm_delete {
            self.start_delete(pending);
        } else {
            self.delete.pending = Some(pending);
        }
    }

    /// Offers to replace the ticked duplicates by hard links to the copies that are kept.
    pub fn request_hardlink(&mut self, ids: Vec<NodeId>) {
        let Some(data) = self.data.clone() else { return };
        let ids: Vec<NodeId> = ids.into_iter().filter(|i| data.dup_keep.contains_key(i)).collect();
        if ids.is_empty() {
            return;
        }
        self.delete.pending = Some(Pending {
            size: ids.iter().map(|&i| data.out.tree.node(i).size).sum(),
            ids,
            permanent: false,
            empty_bin: false,
            link: true,
            system: false,
        });
    }

    pub fn request_empty_bin(&mut self, bins: Vec<NodeId>) {
        let Some(data) = self.data.clone() else { return };
        let size = bins.iter().map(|&i| data.out.tree.node(i).size).sum();
        self.delete.pending = Some(Pending {
            ids: bins,
            permanent: true,
            empty_bin: true,
            link: false,
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
        let jobs: Vec<(NodeId, String, u64)> = p
            .ids
            .iter()
            .map(|&i| (i, tree.path(i), weight(tree.node(i).size, tree.node(i).files)))
            .collect();
        let keep_paths: Vec<String> = if p.link {
            p.ids.iter().map(|i| tree.path(data.dup_keep[i])).collect()
        } else {
            Vec::new()
        };
        let bin_children: Vec<NodeId> = if p.empty_bin {
            p.ids.iter().flat_map(|&b| tree.children(b).to_vec()).collect()
        } else {
            Vec::new()
        };
        let (tx, rx) = channel();
        let done = Arc::new(AtomicU64::new(0));
        let current = Arc::new(Mutex::new(String::new()));
        let total = jobs.iter().map(|j| j.2).sum::<u64>().max(1);
        let (d2, c2) = (done.clone(), current.clone());
        let (permanent, empty_bin, link) = (p.permanent, p.empty_bin, p.link);
        std::thread::spawn(move || {
            let mut out = Outcome {
                removed: Vec::new(),
                linked: Vec::new(),
                failed: Vec::new(),
            };
            if link {
                for ((id, path, w), keep) in jobs.into_iter().zip(keep_paths) {
                    *c2.lock().unwrap() = path.clone();
                    match spacetree_core::dupes::replace_with_hardlink(keep.as_ref(), path.as_ref()) {
                        Ok(()) => out.linked.push(id),
                        Err(e) => out.failed.push((path, e.to_string())),
                    }
                    d2.fetch_add(w, Relaxed);
                }
            } else if empty_bin {
                match winint::empty_recycle_bin() {
                    Ok(()) => out.removed = bin_children,
                    Err(e) => out.failed.push(("Корзина".into(), e)),
                }
            } else {
                for (id, path, w) in jobs {
                    *c2.lock().unwrap() = path.clone();
                    let p = Path::new(&path);
                    let before = d2.load(Relaxed);
                    let res = if permanent {
                        if p.is_dir() && !p.is_symlink() {
                            remove_dir_progress(p, &d2, &c2)
                        } else {
                            remove_file_progress(p, &d2)
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
                    // The estimate may differ from reality: land exactly on the job's share.
                    d2.store(before + w, Relaxed);
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
                        "delete finished: {} removed, {} linked, {} failed",
                        o.removed.len(),
                        o.linked.len(),
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
        let mut link_toast = None;
        if !outcome.linked.is_empty() {
            let linked: std::collections::HashSet<NodeId> = outcome.linked.iter().copied().collect();
            let saved: u64 = linked.iter().map(|&i| data.out.tree.node(i).size).sum();
            if let Some(sets) = &mut data.dup_sets {
                for s in sets.iter_mut() {
                    s.items.retain(|i| !linked.contains(i));
                }
                sets.retain(|s| s.items.len() > 1);
            }
            data.merge_dups();
            data.disk = spacetree_core::disk_space(&data.root_path);
            link_toast = Some(format!(
                "Заменено жёсткими ссылками: {} · освобождено {}",
                outcome.linked.len(),
                human_size(saved)
            ));
        }
        let before = data.out.tree.root_stats().0;
        for &id in &outcome.removed {
            data.out.tree.remove(id);
        }
        let freed = before - data.out.tree.root_stats().0;
        if !outcome.removed.is_empty() {
            data.reanalyze();
        }
        self.tree_changed();
        if let Some(t) = link_toast {
            self.toast(t, ToastKind::Success);
        }
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
                    ui.add(
                        egui::ProgressBar::new((done as f64 / total.max(1) as f64).min(1.0) as f32).show_percentage(),
                    );
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
            } else if p.link {
                format!("Заменить копии жёсткими ссылками ({})?", p.ids.len())
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
                if p.link {
                    ui.label(
                        "Все имена файлов останутся на местах, программы подмены не заметят, но на диске                          будет храниться одна копия данных. Права и атрибуты копий станут как у сохраняемого файла;                          изменение одного из файлов изменит и остальные. Работает только в пределах одного NTFS-тома.",
                    );
                } else {
                    ui.checkbox(&mut p.permanent, "Удалить безвозвратно, минуя корзину");
                }
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
            if p.link {
            } else if p.permanent {
                ui.label(RichText::new("Восстановить файлы после этого будет нельзя.").color(theme::REVIEW));
            } else if !p.empty_bin {
                ui.label(RichText::new("Файлы попадут в корзину — их можно будет восстановить.").weak());
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let label = if p.empty_bin {
                    "Очистить"
                } else if p.link {
                    "Заменить ссылками"
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
