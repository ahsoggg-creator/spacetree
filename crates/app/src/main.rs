#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod theme;
mod treemap;
mod views;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Color32, Key, Layout, RichText, Sense, Ui, vec2};
use spacetree_core::cleanup::{self, Group};
use spacetree_core::stats::{TypeStat, human_size, top_files, type_stats};
use spacetree_core::{Backend, NodeId, Progress, ROOT, ScanError, ScanOutput, Used, export};

const TOP_N: usize = 1000;
const SEARCH_LIMIT: usize = 5000;
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(200);

/// Everything computed in the background after a scan.
pub struct Loaded {
    pub out: ScanOutput,
    pub top: Vec<NodeId>,
    pub types: Vec<TypeStat>,
    pub cleanup: Vec<Group>,
    pub cleanup_total: u64,
    /// `(total, free)` of the scanned volume.
    pub disk: Option<(u64, u64)>,
    pub analysis: Duration,
}

struct Job {
    progress: Arc<Progress>,
    rx: Receiver<Result<Loaded, ScanError>>,
    started: Instant,
    path: String,
}

#[derive(PartialEq, Clone, Copy)]
pub enum Tab {
    Overview,
    Types,
    Top,
    Cleanup,
}

#[derive(PartialEq, Clone, Copy)]
pub enum SortKey {
    Size,
    Name,
    Files,
    Modified,
}

pub struct TmCache {
    pub root: NodeId,
    pub size: egui::Vec2,
    pub rects: Vec<treemap::TmRect>,
    /// Per rect: fill color and lower-cased extension ("" for folders).
    pub colors: Vec<Color32>,
    pub exts: Vec<String>,
}

struct DriveInfo {
    path: String,
    space: Option<(u64, u64)>,
}

/// Something the user asked for from a row, a tile or a context menu; applied after drawing.
#[derive(Clone, Copy)]
pub enum Action {
    Select(NodeId),
    /// Select and show in the overview tab (tree + treemap focused on its folder).
    Show(NodeId),
    Toggle(NodeId),
    Reveal(NodeId),
    Open(NodeId),
    CopyPath(NodeId),
}

pub struct App {
    path_text: String,
    drives: Vec<DriveInfo>,
    backend: Backend,
    /// Raw MFT reading works (Windows + administrator): scans of whole drives are much faster.
    mft_ready: bool,
    auto_scan: bool,
    job: Option<Job>,
    pub data: Option<Arc<Loaded>>,
    message: String,
    status: Arc<Mutex<String>>,
    pub tab: Tab,

    pub selected: Option<NodeId>,
    pub tm_root: NodeId,
    pub expanded: HashSet<NodeId>,
    pub rows: Vec<(NodeId, u16)>,
    pub rows_dirty: bool,
    pub scroll_to: Option<usize>,
    pub tm: Option<TmCache>,
    pub tm_menu: Option<NodeId>,
    pub highlight_ext: Option<String>,
    pub sort: SortKey,
    pub sort_desc: bool,

    pub search: String,
    search_done: String,
    search_edited: Option<Instant>,
    pub search_hits: Vec<NodeId>,
    pub search_truncated: bool,

    pub cleanup_open: HashSet<usize>,
    pub cleanup_show: [bool; 3],
}

fn list_drives() -> Vec<DriveInfo> {
    let paths: Vec<String> = if cfg!(windows) {
        ('A'..='Z')
            .map(|c| format!("{c}:\\"))
            .filter(|p| std::path::Path::new(p).exists())
            .collect()
    } else {
        vec!["/".into()]
    };
    paths
        .into_iter()
        .map(|path| DriveInfo {
            space: spacetree_core::disk_space(std::path::Path::new(&path)),
            path,
        })
        .collect()
}

fn now_unix() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs().min(u32::MAX as u64) as u32)
}

/// Case-insensitive substring test; `needle` must already be lower-case.
fn contains_ci(hay: &str, needle: &str) -> bool {
    if needle.is_ascii() {
        let n = needle.as_bytes();
        hay.len() >= n.len() && hay.as_bytes().windows(n.len()).any(|w| w.eq_ignore_ascii_case(n))
    } else {
        hay.to_lowercase().contains(needle)
    }
}

impl App {
    fn new(cc: &eframe::CreationContext) -> Self {
        theme::install(&cc.egui_ctx);
        let drives = list_drives();
        let arg = std::env::args().nth(1);
        let mft_ready = cfg!(windows)
            && drives.first().is_some_and(|d| {
                spacetree_core::select_backend(std::path::Path::new(&d.path), Backend::Auto)
                    .is_ok_and(|s| s.used == Used::Mft)
            });
        App {
            path_text: arg
                .clone()
                .or_else(|| drives.first().map(|d| d.path.clone()))
                .unwrap_or_default(),
            auto_scan: arg.is_some(),
            drives,
            backend: Backend::Auto,
            mft_ready,
            job: None,
            data: None,
            message: String::new(),
            status: Arc::default(),
            tab: Tab::Overview,
            selected: None,
            tm_root: ROOT,
            expanded: HashSet::new(),
            rows: Vec::new(),
            rows_dirty: false,
            scroll_to: None,
            tm: None,
            tm_menu: None,
            highlight_ext: None,
            sort: SortKey::Size,
            sort_desc: true,
            search: String::new(),
            search_done: String::new(),
            search_edited: None,
            search_hits: Vec::new(),
            search_truncated: false,
            cleanup_open: HashSet::new(),
            cleanup_show: [true; 3],
        }
    }

    fn start_scan(&mut self, ctx: &egui::Context) {
        if self.job.is_some() {
            return;
        }
        let mut text = self.path_text.trim().to_owned();
        if cfg!(windows) && text.len() == 2 && text.ends_with(':') {
            text.push('\\'); // `C:` alone means "current directory on C:"
            self.path_text = text.clone();
        }
        let path = PathBuf::from(&text);
        let backend = self.backend;
        let progress = Arc::new(Progress::default());
        let (tx, rx) = channel();
        let (p, ctx2) = (progress.clone(), ctx.clone());
        std::thread::spawn(move || {
            let res = spacetree_core::scan(&path, backend, &p).map(|out| {
                let t = Instant::now();
                let tree = &out.tree;
                let ((top, types), groups) = rayon::join(
                    || rayon::join(|| top_files(tree, TOP_N), || type_stats(tree, ROOT)),
                    || cleanup::find(tree, now_unix()),
                );
                Loaded {
                    cleanup_total: groups.iter().map(|g| g.size).sum(),
                    cleanup: groups,
                    top,
                    types,
                    disk: spacetree_core::disk_space(&path),
                    analysis: t.elapsed(),
                    out,
                }
            });
            let _ = tx.send(res);
            ctx2.request_repaint();
        });
        self.job = Some(Job {
            progress,
            rx,
            started: Instant::now(),
            path: text,
        });
        self.message.clear();
    }

    fn poll_job(&mut self) {
        let Some(job) = &self.job else { return };
        match job.rx.try_recv() {
            Ok(Ok(loaded)) => {
                self.data = Some(Arc::new(loaded));
                self.job = None;
                self.selected = None;
                self.tm_root = ROOT;
                self.tm = None;
                self.expanded = HashSet::from([ROOT]);
                self.rows_dirty = true;
                self.search_done.clear();
                self.search_hits.clear();
                self.cleanup_open.clear();
                self.highlight_ext = None;
                self.drives = list_drives();
            }
            Ok(Err(e)) => {
                self.message = e.to_string();
                self.job = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.message = "поток сканирования завершился аварийно".into();
                self.job = None;
            }
        }
    }

    pub fn rebuild_rows(&mut self) {
        self.rows.clear();
        self.rows_dirty = false;
        let Some(data) = self.data.clone() else { return };
        let tree = &data.out.tree;
        let mut stack = vec![(ROOT, 0u16)];
        while let Some((id, depth)) = stack.pop() {
            self.rows.push((id, depth));
            if tree.is_dir(id) && self.expanded.contains(&id) {
                let mut ch = tree.children(id).to_vec(); // size, descending
                match self.sort {
                    SortKey::Size => {}
                    SortKey::Name => ch.sort_by_cached_key(|&c| tree.name(c).to_lowercase()),
                    SortKey::Files => ch.sort_by_key(|&c| std::cmp::Reverse(tree.node(c).files)),
                    SortKey::Modified => ch.sort_by_key(|&c| std::cmp::Reverse(tree.node(c).mtime)),
                }
                let natural_desc = self.sort != SortKey::Name;
                if natural_desc != self.sort_desc {
                    ch.reverse();
                }
                // pushed in reverse so that the first child is popped (shown) first
                stack.extend(ch.into_iter().rev().map(|c| (c, depth + 1)));
            }
        }
    }

    /// Selects a node everywhere: expands its ancestors, points the treemap at its folder, scrolls the tree.
    pub fn select(&mut self, id: NodeId) {
        self.select_keep_view(id);
        let Some(data) = &self.data else { return };
        let tree = &data.out.tree;
        self.tm_root = if tree.is_dir(id) {
            id
        } else {
            tree.parent(id).unwrap_or(ROOT)
        };
    }

    /// Like `select`, but leaves the treemap where it is.
    pub fn select_keep_view(&mut self, id: NodeId) {
        let Some(data) = self.data.clone() else { return };
        for a in data.out.tree.ancestors(id) {
            if a != id {
                self.expanded.insert(a);
            }
        }
        self.selected = Some(id);
        self.rebuild_rows();
        self.scroll_to = self.rows.iter().position(|&(r, _)| r == id);
    }

    pub fn tm_up(&mut self) {
        let Some(data) = &self.data else { return };
        if self.tm_root != ROOT {
            self.tm_root = data.out.tree.parent(self.tm_root).unwrap_or(ROOT);
        }
    }

    pub fn apply(&mut self, ctx: &egui::Context, action: Action) {
        let Some(data) = self.data.clone() else { return };
        let tree = &data.out.tree;
        match action {
            Action::Select(id) => self.select_keep_view(id),
            Action::Show(id) => {
                self.tab = Tab::Overview;
                self.select(id);
            }
            Action::Toggle(id) => {
                if !self.expanded.remove(&id) {
                    self.expanded.insert(id);
                }
                self.rows_dirty = true;
            }
            Action::Reveal(id) => reveal(&tree.path(id)),
            Action::Open(id) => open_path(&tree.path(id)),
            Action::CopyPath(id) => ctx.copy_text(tree.path(id)),
        }
    }

    fn update_search(&mut self, ctx: &egui::Context) {
        let q = self.search.trim().to_lowercase();
        if q == self.search_done {
            return;
        }
        if let Some(t) = self.search_edited
            && t.elapsed() < SEARCH_DEBOUNCE
        {
            ctx.request_repaint_after(SEARCH_DEBOUNCE);
            return;
        }
        self.search_done = q.clone();
        self.search_hits.clear();
        self.search_truncated = false;
        let Some(data) = self.data.clone() else { return };
        if q.is_empty() {
            return;
        }
        use rayon::prelude::*;
        let tree = &data.out.tree;
        let mut hits: Vec<NodeId> = (1..tree.len() as NodeId)
            .into_par_iter()
            .filter(|&i| contains_ci(tree.name(i), &q))
            .collect();
        hits.par_sort_unstable_by_key(|&i| std::cmp::Reverse(tree.node(i).size));
        self.search_truncated = hits.len() > SEARCH_LIMIT;
        hits.truncate(SEARCH_LIMIT);
        self.search_hits = hits;
    }

    pub fn search_active(&self) -> bool {
        !self.search_done.is_empty()
    }

    fn export_csv(&mut self, data: Arc<Loaded>) {
        let Some(p) = rfd::FileDialog::new().set_file_name("spacetree.csv").save_file() else {
            return;
        };
        let status = self.status.clone();
        *status.lock().unwrap() = "Экспорт CSV…".into();
        std::thread::spawn(move || {
            let res = std::fs::File::create(&p)
                .map(std::io::BufWriter::new)
                .and_then(|mut w| {
                    export::write_csv(&data.out.tree, &mut w)?;
                    std::io::Write::flush(&mut w)
                });
            *status.lock().unwrap() = match res {
                Ok(()) => format!("CSV сохранён: {}", p.display()),
                Err(e) => format!("Ошибка записи CSV: {e}"),
            };
        });
    }

    fn toolbar(&mut self, ui: &mut Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("◼ spacetree").strong().size(16.0).color(theme::ACCENT));
            ui.add_space(8.0);
            let scanning = self.job.is_some();
            ui.add_enabled_ui(!scanning, |ui| {
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.path_text)
                        .hint_text("Диск или папка")
                        .desired_width(260.0),
                );
                if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    let ctx = ui.ctx().clone();
                    self.start_scan(&ctx);
                }
                egui::ComboBox::from_id_salt("drives")
                    .selected_text("Диски")
                    .show_ui(ui, |ui| {
                        for d in &self.drives {
                            let label = match d.space {
                                Some((t, f)) => format!("{}   свободно {} из {}", d.path, human_size(f), human_size(t)),
                                None => d.path.clone(),
                            };
                            if ui.selectable_label(false, label).clicked() {
                                self.path_text = d.path.clone();
                            }
                        }
                    });
                if ui.button("Папка…").on_hover_text("Выбрать папку").clicked()
                    && let Some(p) = rfd::FileDialog::new().pick_folder()
                {
                    self.path_text = p.to_string_lossy().into_owned();
                }
                egui::ComboBox::from_id_salt("backend")
                    .selected_text(match self.backend {
                        Backend::Auto => "Авто",
                        Backend::Walk => "Обход папок",
                        Backend::Mft => "MFT",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.backend, Backend::Auto, "Авто")
                            .on_hover_text("MFT для целого NTFS-диска при правах администратора, иначе обход папок");
                        ui.selectable_value(&mut self.backend, Backend::Walk, "Обход папок");
                        ui.selectable_value(&mut self.backend, Backend::Mft, "MFT (только корень NTFS-диска)");
                    })
                    .response
                    .on_hover_text("Способ сканирования");
            });
            if scanning {
                if ui.button("⏹ Отмена").clicked()
                    && let Some(j) = &self.job
                {
                    j.progress.cancel();
                }
            } else if theme::primary_button(ui, "▶ Сканировать").clicked() {
                let ctx = ui.ctx().clone();
                self.start_scan(&ctx);
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(data) = self.data.clone() {
                    if ui.button("⬇ CSV").on_hover_text("Экспорт всего дерева в CSV").clicked() {
                        self.export_csv(data);
                    }
                    let id = egui::Id::new("search");
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.search)
                            .id(id)
                            .hint_text("🔍 Поиск по имени (Ctrl+F)")
                            .desired_width(220.0),
                    );
                    if resp.changed() {
                        self.search_edited = Some(Instant::now());
                        self.tab = Tab::Overview;
                    }
                    if ui.input(|i| i.modifiers.command && i.key_pressed(Key::F)) {
                        resp.request_focus();
                    }
                    if resp.has_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
                        self.search.clear();
                    }
                }
            });
        });
        ui.add_space(2.0);
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            if let Some(d) = &self.data {
                let (size, alloc, files) = d.out.tree.root_stats();
                ui.label(format!(
                    "Файлов: {files} · папок: {} · размер: {} · на диске: {}",
                    d.out.tree.dir_count().saturating_sub(1),
                    human_size(size),
                    human_size(alloc),
                ));
                ui.separator();
                let used = match d.out.selection.used {
                    Used::Mft => "MFT",
                    Used::Walk => "обход папок",
                };
                ui.label(format!("{used} · {:.2} с", d.out.elapsed.as_secs_f64()))
                    .on_hover_text(format!(
                        "Способ: {}\nАнализ после сканирования: {:.2} с",
                        d.out.selection.reason,
                        d.analysis.as_secs_f64()
                    ));
                if d.out.errors > 0 {
                    ui.separator();
                    ui.label(RichText::new(format!("⚠ нет доступа: {}", d.out.errors)).color(theme::LIKELY))
                        .on_hover_text(
                            d.out
                                .error_samples
                                .iter()
                                .take(20)
                                .cloned()
                                .collect::<Vec<_>>()
                                .join("\n"),
                        );
                }
            }
            if !self.message.is_empty() {
                ui.colored_label(ui.visuals().error_fg_color, &self.message);
            }
            let st = self.status.lock().unwrap().clone();
            if !st.is_empty() {
                ui.label(st);
            }
            if let Some((total, free)) = self.data.as_ref().and_then(|d| d.disk) {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(100.0, 8.0), Sense::hover());
                    theme::bar(ui, r, 1.0 - free as f32 / total.max(1) as f32, theme::ACCENT);
                    ui.label(format!("Свободно {} из {}", human_size(free), human_size(total)));
                });
            }
        });
    }

    fn welcome(&mut self, ui: &mut Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.12).max(16.0));
            ui.label(RichText::new("Что занимает место?").size(26.0).strong());
            ui.label(
                RichText::new("Выберите диск или папку — spacetree покажет, куда ушло место и что можно удалить.")
                    .weak(),
            );
        });
        ui.add_space(20.0);

        // drive cards, centered, wrapping into rows
        let (card_w, card_h, gap) = (240.0, 84.0, 12.0);
        let avail = ui.available_width();
        let cols = (((avail + gap) / (card_w + gap)).floor() as usize).clamp(1, self.drives.len().max(1));
        let rows = self.drives.len().div_ceil(cols);
        let (area, _) = ui.allocate_exact_size(vec2(avail, rows as f32 * (card_h + gap)), Sense::hover());
        let mut chosen = None;
        for (i, d) in self.drives.iter().enumerate() {
            let (row, col) = (i / cols, i % cols);
            let in_row = (self.drives.len() - row * cols).min(cols) as f32;
            let left = area.left() + (avail - in_row * (card_w + gap) + gap) / 2.0;
            let rect = egui::Rect::from_min_size(
                egui::pos2(
                    left + col as f32 * (card_w + gap),
                    area.top() + row as f32 * (card_h + gap),
                ),
                vec2(card_w, card_h),
            );
            let resp = ui
                .interact(rect, egui::Id::new(("drive", i)), Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text("Сканировать");
            let v = ui.visuals();
            let fill = if resp.hovered() {
                v.widgets.hovered.weak_bg_fill
            } else {
                v.window_fill
            };
            let stroke = if resp.hovered() {
                egui::Stroke::new(1.0, theme::ACCENT)
            } else {
                v.widgets.noninteractive.bg_stroke
            };
            let (text, weak) = (v.text_color(), v.weak_text_color());
            let p = ui.painter();
            p.rect(rect, 10.0, fill, stroke, egui::StrokeKind::Inside);
            let inner = rect.shrink(14.0);
            p.text(
                inner.left_top(),
                egui::Align2::LEFT_TOP,
                &d.path,
                egui::FontId::proportional(18.0),
                text,
            );
            if let Some((total, free)) = d.space {
                let frac = (total - free) as f32 / total.max(1) as f32;
                let bar = egui::Rect::from_min_size(inner.left_top() + vec2(0.0, 28.0), vec2(inner.width(), 8.0));
                theme::bar(ui, bar, frac, if frac > 0.9 { theme::REVIEW } else { theme::ACCENT });
                ui.painter().text(
                    inner.left_bottom(),
                    egui::Align2::LEFT_BOTTOM,
                    format!("свободно {} из {}", human_size(free), human_size(total)),
                    egui::FontId::proportional(12.5),
                    weak,
                );
            }
            if resp.clicked() {
                chosen = Some(d.path.clone());
            }
        }

        ui.vertical_centered(|ui| {
            ui.add_space(6.0);
            if ui.button("Выбрать папку…").clicked()
                && let Some(p) = rfd::FileDialog::new().pick_folder()
            {
                chosen = Some(p.to_string_lossy().into_owned());
            }
            ui.add_space(24.0);
            if cfg!(windows) && !self.mft_ready {
                theme::card(ui).show(ui, |ui| {
                    ui.set_max_width(560.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("⚡ Быстрый режим").strong().color(theme::LIKELY));
                        ui.label(
                            RichText::new(
                                "Без прав администратора диск сканируется обходом папок. С правами администратора \
                                 spacetree читает таблицу MFT напрямую — для целых дисков это обычно намного быстрее.",
                            )
                            .weak(),
                        );
                        ui.add_space(4.0);
                        if ui.button("Перезапустить от администратора").clicked() {
                            match relaunch_elevated(self.path_text.trim()) {
                                Ok(()) => ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close),
                                Err(e) => self.message = e,
                            }
                        }
                    });
                });
            }
        });
        if let Some(p) = chosen {
            self.path_text = p;
            let ctx = ui.ctx().clone();
            self.start_scan(&ctx);
        }
    }

    fn scanning(&mut self, ui: &mut Ui) {
        let Some(job) = &self.job else { return };
        let p = &job.progress;
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.25).max(16.0));
            ui.label(RichText::new(format!("Сканирование {}", job.path)).size(20.0).strong());
            ui.add_space(12.0);
            let building = p.phase.load(Relaxed) == spacetree_core::progress::PHASE_BUILDING;
            let bar = match p.fraction() {
                Some(f) if !building => egui::ProgressBar::new(f).show_percentage(),
                _ => egui::ProgressBar::new(1.0).animate(true),
            };
            ui.add(bar.desired_width(420.0).text(if building {
                "Построение дерева…"
            } else {
                ""
            }));
            ui.add_space(12.0);
            ui.label(
                RichText::new(format!(
                    "{} файлов · {} · {:.1} с",
                    p.files.load(Relaxed),
                    human_size(p.bytes.load(Relaxed)),
                    job.started.elapsed().as_secs_f32()
                ))
                .size(15.0),
            );
            let errors = p.errors.load(Relaxed);
            if errors > 0 {
                ui.label(RichText::new(format!("нет доступа: {errors}")).weak());
            }
            ui.add_space(16.0);
            if ui.button("⏹ Отмена").clicked() {
                p.cancel();
            }
        });
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }

    fn tabs(&mut self, ui: &mut Ui) {
        let Some(data) = self.data.clone() else { return };
        ui.horizontal(|ui| {
            let cleanup = format!("Можно удалить · {}", human_size(data.cleanup_total));
            for (tab, label) in [
                (Tab::Overview, "Обзор"),
                (Tab::Types, "Типы файлов"),
                (Tab::Top, "Крупнейшие файлы"),
                (Tab::Cleanup, cleanup.as_str()),
            ] {
                if ui.add(egui::Button::selectable(self.tab == tab, label)).clicked() {
                    self.tab = tab;
                }
            }
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_job();
        if self.auto_scan {
            self.auto_scan = false;
            self.start_scan(&ctx);
        }
        if ctx.input(|i| i.key_pressed(Key::F5)) && self.data.is_some() {
            self.start_scan(&ctx);
        }
        self.update_search(&ctx);

        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        if self.job.is_none() && self.data.is_some() {
            egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        }
        if self.job.is_some() {
            egui::CentralPanel::default().show(ui, |ui| self.scanning(ui));
            return;
        }
        if self.data.is_none() {
            egui::CentralPanel::default().show(ui, |ui| {
                if !self.message.is_empty() {
                    ui.colored_label(ui.visuals().error_fg_color, &self.message);
                }
                self.welcome(ui)
            });
            return;
        }
        egui::Panel::top("tabs").show(ui, |ui| {
            ui.add_space(2.0);
            self.tabs(ui);
            ui.add_space(2.0);
        });
        match self.tab {
            Tab::Overview => {
                egui::Panel::left("tree")
                    .resizable(true)
                    .default_size(660.0)
                    .min_size(320.0)
                    .show(ui, |ui| {
                        if self.search_active() {
                            self.search_view(ui)
                        } else {
                            self.tree_view(ui)
                        }
                    });
                egui::CentralPanel::default().show(ui, |ui| self.treemap_view(ui));
            }
            Tab::Types => {
                egui::CentralPanel::default().show(ui, |ui| self.types_view(ui));
            }
            Tab::Top => {
                egui::CentralPanel::default().show(ui, |ui| self.top_view(ui));
            }
            Tab::Cleanup => {
                egui::CentralPanel::default().show(ui, |ui| self.cleanup_view(ui));
            }
        }
    }
}

fn reveal(path: &str) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("explorer")
            .raw_arg(format!("/select,\"{path}\""))
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let dir = std::path::Path::new(path)
            .parent()
            .unwrap_or(std::path::Path::new(path));
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}

fn open_path(path: &str) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(not(windows))]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

/// Starts this executable again through the UAC prompt, asking it to scan `path`.
fn relaunch_elevated(path: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let w = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
        let (verb, file) = (w("runas"), w(&exe.to_string_lossy()));
        // A trailing backslash before the closing quote would escape it (`"C:\"`): double it.
        let arg = if path.ends_with('\\') {
            format!("{path}\\")
        } else {
            path.to_owned()
        };
        let params = w(&format!("\"{arg}\""));
        // SAFETY: all strings are NUL-terminated and outlive the call; no window handle or directory.
        let h = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                params.as_ptr(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        if h as isize > 32 {
            Ok(())
        } else {
            Err("не удалось перезапуститься от администратора (запрос UAC отклонён?)".into())
        }
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err("доступно только в Windows".into())
    }
}

fn main() -> eframe::Result {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1360.0, 860.0])
            .with_min_inner_size([900.0, 560.0])
            .with_title("spacetree — анализ места на диске"),
        ..Default::default()
    };
    eframe::run_native("spacetree", opts, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
