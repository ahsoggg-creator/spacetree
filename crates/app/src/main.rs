mod treemap;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2};
use spacetree_core::stats::{TypeStat, extension, human_size, top_files, type_stats};
use spacetree_core::{Backend, Kind, NodeId, Progress, ROOT, ScanError, ScanOutput, export};

const ROW_H: f32 = 20.0;
const INDENT: f32 = 14.0;
const TOP_N: usize = 1000;

struct Loaded {
    out: ScanOutput,
    top: Vec<NodeId>,
    types: Vec<TypeStat>,
}

struct Job {
    progress: Arc<Progress>,
    rx: Receiver<Result<Loaded, ScanError>>,
    started: Instant,
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Tree,
    Types,
    Top,
}

struct TmCache {
    root: NodeId,
    size: egui::Vec2,
    rects: Vec<treemap::TmRect>,
}

struct App {
    path_text: String,
    drives: Vec<String>,
    backend: Backend,
    job: Option<Job>,
    data: Option<Arc<Loaded>>,
    message: String,
    status: Arc<Mutex<String>>,
    tab: Tab,

    selected: Option<NodeId>,
    tm_root: NodeId,
    expanded: HashSet<NodeId>,
    rows: Vec<(NodeId, u16)>,
    rows_dirty: bool,
    scroll_to: Option<usize>,
    tm: Option<TmCache>,
    colors: HashMap<String, Color32>,
}

fn list_drives() -> Vec<String> {
    if cfg!(windows) {
        ('A'..='Z')
            .map(|c| format!("{c}:\\"))
            .filter(|p| std::path::Path::new(p).exists())
            .collect()
    } else {
        vec!["/".into()]
    }
}

impl App {
    fn new() -> Self {
        let drives = list_drives();
        App {
            path_text: drives.first().cloned().unwrap_or_default(),
            drives,
            backend: Backend::Auto,
            job: None,
            data: None,
            message: String::new(),
            status: Arc::default(),
            tab: Tab::Tree,
            selected: None,
            tm_root: ROOT,
            expanded: HashSet::new(),
            rows: Vec::new(),
            rows_dirty: false,
            scroll_to: None,
            tm: None,
            colors: HashMap::new(),
        }
    }

    fn start_scan(&mut self, ctx: &egui::Context) {
        if self.job.is_some() {
            return;
        }
        let path = PathBuf::from(self.path_text.trim());
        let backend = self.backend;
        let progress = Arc::new(Progress::default());
        let (tx, rx) = channel();
        let (p, ctx2) = (progress.clone(), ctx.clone());
        std::thread::spawn(move || {
            let res = spacetree_core::scan(&path, backend, &p).map(|out| {
                let top = top_files(&out.tree, TOP_N);
                let types = type_stats(&out.tree, ROOT);
                Loaded { out, top, types }
            });
            let _ = tx.send(res);
            ctx2.request_repaint();
        });
        self.job = Some(Job {
            progress,
            rx,
            started: Instant::now(),
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

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        let Some(data) = &self.data else { return };
        let tree = &data.out.tree;
        let mut stack = vec![(ROOT, 0u16)];
        while let Some((id, depth)) = stack.pop() {
            self.rows.push((id, depth));
            if tree.is_dir(id) && self.expanded.contains(&id) {
                stack.extend(tree.children(id).iter().rev().map(|&c| (c, depth + 1)));
            }
        }
        self.rows_dirty = false;
    }

    /// Selects a node everywhere: expands its ancestors, points the treemap at its folder, scrolls the tree.
    fn select(&mut self, id: NodeId) {
        let Some(data) = &self.data else { return };
        let tree = &data.out.tree;
        for a in tree.ancestors(id) {
            if a != id {
                self.expanded.insert(a);
            }
        }
        self.tm_root = if tree.is_dir(id) {
            id
        } else {
            tree.parent(id).unwrap_or(ROOT)
        };
        self.selected = Some(id);
        self.rebuild_rows();
        self.scroll_to = self.rows.iter().position(|&(r, _)| r == id);
    }

    fn color_for(&mut self, ext: &str) -> Color32 {
        *self.colors.entry(ext.to_owned()).or_insert_with(|| {
            let mut h: u32 = 2166136261;
            for b in ext.bytes() {
                h = (h ^ b as u32).wrapping_mul(16777619);
            }
            let hue = (h % 360) as f32 / 360.0;
            let c: egui::ecolor::Hsva = egui::ecolor::Hsva::new(hue, 0.55, 0.78, 1.0);
            c.into()
        })
    }

    fn top_bar(&mut self, ui: &mut Ui) {
        ui.horizontal_wrapped(|ui| {
            let scanning = self.job.is_some();
            ui.add_enabled_ui(!scanning, |ui| {
                ui.label("Путь:");
                ui.add(egui::TextEdit::singleline(&mut self.path_text).desired_width(280.0));
                egui::ComboBox::from_id_salt("drives")
                    .selected_text("Диск")
                    .show_ui(ui, |ui| {
                        for d in self.drives.clone() {
                            if ui.selectable_label(false, &d).clicked() {
                                self.path_text = d;
                            }
                        }
                    });
                if ui.button("Папка…").clicked()
                    && let Some(p) = rfd::FileDialog::new().pick_folder()
                {
                    self.path_text = p.to_string_lossy().into_owned();
                }
                egui::ComboBox::from_id_salt("backend")
                    .selected_text(match self.backend {
                        Backend::Auto => "auto",
                        Backend::Walk => "walk",
                        Backend::Mft => "mft",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.backend, Backend::Auto, "auto");
                        ui.selectable_value(&mut self.backend, Backend::Walk, "walk");
                        ui.selectable_value(&mut self.backend, Backend::Mft, "mft");
                    });
            });
            if scanning {
                if ui.button("Отмена").clicked()
                    && let Some(j) = &self.job
                {
                    j.progress.cancel();
                }
            } else if ui.button("Сканировать").clicked() {
                let ctx = ui.ctx().clone();
                self.start_scan(&ctx);
            }
            if let Some(data) = self.data.clone() {
                if ui.button("Экспорт CSV…").clicked()
                    && let Some(p) = rfd::FileDialog::new().set_file_name("spacetree.csv").save_file()
                {
                    let (status, data) = (self.status.clone(), data.clone());
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
                #[cfg(windows)]
                if ui
                    .add_enabled(self.selected.is_some(), egui::Button::new("Показать в проводнике"))
                    .clicked()
                    && let Some(id) = self.selected
                {
                    use std::os::windows::process::CommandExt;
                    let path = data.out.tree.path(id);
                    let _ = std::process::Command::new("explorer")
                        .raw_arg(format!("/select,\"{path}\""))
                        .spawn();
                }
            }
        });
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            if let Some(job) = &self.job {
                let p = &job.progress;
                let building = p.phase.load(Relaxed) == spacetree_core::progress::PHASE_BUILDING;
                let txt = if building {
                    "Построение дерева…"
                } else {
                    "Сканирование…"
                };
                match p.fraction() {
                    Some(f) if !building => ui.add(egui::ProgressBar::new(f).desired_width(180.0).text(txt)),
                    _ => ui.add(egui::ProgressBar::new(1.0).animate(true).desired_width(180.0).text(txt)),
                };
                ui.label(format!(
                    "файлов: {} · {} · ошибок: {} · {:.1} с",
                    p.files.load(Relaxed),
                    human_size(p.bytes.load(Relaxed)),
                    p.errors.load(Relaxed),
                    job.started.elapsed().as_secs_f32()
                ));
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
            } else if let Some(d) = &self.data {
                let (size, alloc, files) = d.out.tree.root_stats();
                ui.label(format!(
                    "[{}] файлов: {files} · папок: {} · размер: {} · на диске: {} · {:.2} с · ошибок доступа: {}",
                    d.out.selection.used.as_str(),
                    d.out.tree.dir_count().saturating_sub(1),
                    human_size(size),
                    human_size(alloc),
                    d.out.elapsed.as_secs_f64(),
                    d.out.errors
                ))
                .on_hover_text(format!(
                    "Бэкенд: {}\n{}",
                    d.out.selection.reason,
                    d.out
                        .error_samples
                        .iter()
                        .take(15)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("\n")
                ));
            }
            if !self.message.is_empty() {
                ui.colored_label(Color32::LIGHT_RED, &self.message);
            }
            let st = self.status.lock().unwrap().clone();
            if !st.is_empty() {
                ui.label(st);
            }
        });
    }

    fn tree_view(&mut self, ui: &mut Ui) {
        let Some(data) = self.data.clone() else { return };
        let tree = &data.out.tree;
        if self.rows_dirty {
            self.rebuild_rows();
        }
        let width = ui.available_width();
        let (w_size, w_pct, w_files) = (90.0, 60.0, 80.0);
        let cols_w = w_size * 2.0 + w_pct + w_files;
        let font = FontId::proportional(13.0);
        let weak = ui.visuals().weak_text_color();
        let text_col = ui.visuals().text_color();

        let (hdr, _) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::hover());
        let pr = ui.painter_at(hdr);
        pr.text(
            hdr.left_center() + vec2(4.0, 0.0),
            Align2::LEFT_CENTER,
            "Имя",
            font.clone(),
            weak,
        );
        let mut x = hdr.right();
        for (w, t) in [
            (w_files, "Файлов"),
            (w_pct, "% родит."),
            (w_size, "На диске"),
            (w_size, "Размер"),
        ] {
            pr.text(
                pos2(x - 6.0, hdr.center().y),
                Align2::RIGHT_CENTER,
                t,
                font.clone(),
                weak,
            );
            x -= w;
        }

        let mut sa = egui::ScrollArea::vertical().auto_shrink([false, false]);
        if let Some(r) = self.scroll_to.take() {
            sa = sa.vertical_scroll_offset((r as f32 * (ROW_H + ui.spacing().item_spacing.y) - 60.0).max(0.0));
        }
        let mut clicked: Option<NodeId> = None;
        let mut toggled: Option<NodeId> = None;
        sa.show_rows(ui, ROW_H, self.rows.len(), |ui, range| {
            for i in range {
                let (id, depth) = self.rows[i];
                let (rect, resp) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::click());
                let painter = ui.painter_at(rect);
                let sel = self.selected == Some(id);
                if sel {
                    painter.rect_filled(rect, 2.0, ui.visuals().selection.bg_fill);
                } else if resp.hovered() {
                    painter.rect_filled(rect, 2.0, ui.visuals().widgets.hovered.weak_bg_fill);
                }
                let nd = tree.node(id);
                let is_dir = nd.kind == Kind::Dir;
                let tx = rect.left() + 4.0 + depth as f32 * INDENT;
                if is_dir && !tree.children(id).is_empty() {
                    let c = pos2(tx + 6.0, rect.center().y);
                    let pts = if self.expanded.contains(&id) {
                        vec![c + vec2(-4.0, -2.5), c + vec2(4.0, -2.5), c + vec2(0.0, 3.0)]
                    } else {
                        vec![c + vec2(-2.5, -4.0), c + vec2(3.0, 0.0), c + vec2(-2.5, 4.0)]
                    };
                    painter.add(Shape::convex_polygon(pts, text_col, Stroke::NONE));
                }
                let (icon, col) = match nd.kind {
                    Kind::Dir => ("", text_col),
                    Kind::File => ("", text_col),
                    Kind::Link => (" ↪", weak),
                };
                let name_clip = Rect::from_min_max(rect.min, pos2(rect.right() - cols_w, rect.bottom()));
                ui.painter_at(name_clip).text(
                    pos2(tx + 16.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    format!("{}{icon}", tree.name(id)),
                    font.clone(),
                    col,
                );
                let parent_size = tree.parent(id).map(|p| tree.node(p).size).unwrap_or(nd.size);
                let pct = if parent_size == 0 {
                    0.0
                } else {
                    nd.size as f64 * 100.0 / parent_size as f64
                };
                let cells = [
                    (w_files, if is_dir { nd.files.to_string() } else { String::new() }),
                    (w_pct, format!("{pct:.1}%")),
                    (w_size, human_size(nd.alloc)),
                    (w_size, human_size(nd.size)),
                ];
                let mut x = rect.right();
                for (w, t) in cells {
                    painter.text(
                        pos2(x - 6.0, rect.center().y),
                        Align2::RIGHT_CENTER,
                        t,
                        font.clone(),
                        text_col,
                    );
                    x -= w;
                }
                if resp.clicked() {
                    let on_arrow = resp.interact_pointer_pos().is_some_and(|p| p.x < tx + 14.0);
                    if is_dir && on_arrow {
                        toggled = Some(id);
                    } else {
                        clicked = Some(id);
                    }
                }
                if resp.double_clicked() && is_dir {
                    toggled = Some(id);
                }
            }
        });
        if let Some(id) = toggled {
            if !self.expanded.remove(&id) {
                self.expanded.insert(id);
            }
            self.rows_dirty = true;
        }
        if let Some(id) = clicked {
            self.selected = Some(id);
            self.tm_root = if tree.is_dir(id) {
                id
            } else {
                tree.parent(id).unwrap_or(ROOT)
            };
        }
    }

    fn treemap_view(&mut self, ui: &mut Ui) {
        let Some(data) = self.data.clone() else { return };
        let tree = &data.out.tree;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.tm_root != ROOT, egui::Button::new("↑ Вверх"))
                .clicked()
            {
                self.tm_root = tree.parent(self.tm_root).unwrap_or(ROOT);
            }
            ui.label(format!(
                "{} — {}",
                tree.path(self.tm_root),
                human_size(tree.node(self.tm_root).size)
            ));
        });
        let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click());
        let area = resp.rect;
        painter.rect_filled(area, 0.0, Color32::from_gray(24));
        if self
            .tm
            .as_ref()
            .is_none_or(|c| c.root != self.tm_root || c.size != area.size())
        {
            let rects = treemap::layout(tree, self.tm_root, Rect::from_min_size(Pos2::ZERO, area.size()));
            self.tm = Some(TmCache {
                root: self.tm_root,
                size: area.size(),
                rects,
            });
        }
        let cache = self.tm.take().unwrap();
        let off = area.min.to_vec2();
        let sel_color = Color32::WHITE;
        let mut hover: Option<NodeId> = None;
        if let Some(p) = resp.hover_pos() {
            hover = treemap::hit(&cache.rects, p - off).map(|r| r.id);
        }
        for r in &cache.rects {
            let rect = r.rect.translate(off);
            if r.nested {
                painter.rect_stroke(rect, 0.0, Stroke::new(1.0, Color32::from_gray(70)), StrokeKind::Inside);
                continue;
            }
            let ext = if tree.is_dir(r.id) {
                "\u{0}dir".to_owned()
            } else {
                extension(tree.name(r.id))
            };
            let col = if ext == "\u{0}dir" {
                Color32::from_rgb(70, 80, 100)
            } else {
                self.color_for(&ext)
            };
            painter.rect_filled(rect.shrink(0.5), 0.0, col);
            if rect.width() > 60.0 && rect.height() > 16.0 {
                painter.with_clip_rect(rect).text(
                    rect.min + vec2(3.0, 2.0),
                    Align2::LEFT_TOP,
                    tree.name(r.id),
                    FontId::proportional(11.0),
                    Color32::BLACK,
                );
            }
        }
        for (id, c) in [(self.selected, sel_color), (hover, Color32::YELLOW)] {
            if let Some(id) = id
                && let Some(r) = cache.rects.iter().find(|r| r.id == id)
            {
                painter.rect_stroke(r.rect.translate(off), 0.0, Stroke::new(2.0, c), StrokeKind::Inside);
            }
        }
        if let Some(h) = hover {
            let nd = tree.node(h);
            resp.clone().on_hover_text(format!(
                "{}\n{} · на диске {}",
                tree.path(h),
                human_size(nd.size),
                human_size(nd.alloc)
            ));
            if resp.clicked() {
                self.tm = Some(cache);
                self.select(h);
                return;
            }
        }
        self.tm = Some(cache);
    }

    fn types_view(&mut self, ui: &mut Ui) {
        let Some(data) = self.data.clone() else { return };
        let total = data.out.tree.root_stats().0.max(1) as f64;
        let types = &data.types;
        ui.horizontal(|ui| {
            for (w, t) in [
                (160.0, "Расширение"),
                (110.0, "Размер"),
                (110.0, "На диске"),
                (90.0, "Файлов"),
                (60.0, "%"),
            ] {
                ui.add_sized([w, 18.0], egui::Label::new(egui::RichText::new(t).weak()));
            }
        });
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, ROW_H, types.len(), |ui, range| {
                for i in range {
                    let t = &types[i];
                    let col = if t.ext.is_empty() {
                        Color32::GRAY
                    } else {
                        self.color_for(&t.ext)
                    };
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                        ui.painter().rect_filled(r, 2.0, col);
                        let name = if t.ext.is_empty() {
                            "(без расширения)".to_owned()
                        } else {
                            format!(".{}", t.ext)
                        };
                        ui.add_sized([140.0, ROW_H], egui::Label::new(name).truncate());
                        ui.add_sized([110.0, ROW_H], egui::Label::new(human_size(t.size)));
                        ui.add_sized([110.0, ROW_H], egui::Label::new(human_size(t.alloc)));
                        ui.add_sized([90.0, ROW_H], egui::Label::new(t.count.to_string()));
                        ui.add_sized(
                            [60.0, ROW_H],
                            egui::Label::new(format!("{:.1}%", t.size as f64 * 100.0 / total)),
                        );
                    });
                }
            });
    }

    fn top_view(&mut self, ui: &mut Ui) {
        let Some(data) = self.data.clone() else { return };
        let tree = &data.out.tree;
        let mut pick = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, ROW_H, data.top.len(), |ui, range| {
                for i in range {
                    let id = data.top[i];
                    ui.horizontal(|ui| {
                        ui.add_sized([90.0, ROW_H], egui::Label::new(human_size(tree.node(id).size)));
                        if ui.selectable_label(self.selected == Some(id), tree.path(id)).clicked() {
                            pick = Some(id);
                        }
                    });
                }
            });
        if let Some(id) = pick {
            self.tab = Tab::Tree;
            self.select(id);
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.poll_job();
        egui::Panel::top("top").show(ui, |ui| self.top_bar(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.data.is_none() {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.centered_and_justified(|ui| {
                    ui.label(if self.job.is_some() {
                        "Идёт сканирование…"
                    } else {
                        "Выберите диск или папку и нажмите «Сканировать»"
                    });
                });
            });
            return;
        }
        egui::Panel::top("tabs").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Tree, "Дерево и treemap");
                ui.selectable_value(&mut self.tab, Tab::Types, "Типы файлов");
                ui.selectable_value(&mut self.tab, Tab::Top, "Крупнейшие файлы");
            });
        });
        match self.tab {
            Tab::Tree => {
                egui::Panel::left("tree")
                    .resizable(true)
                    .default_size(640.0)
                    .show(ui, |ui| self.tree_view(ui));
                egui::CentralPanel::default().show(ui, |ui| self.treemap_view(ui));
            }
            Tab::Types => {
                egui::CentralPanel::default().show(ui, |ui| self.types_view(ui));
            }
            Tab::Top => {
                egui::CentralPanel::default().show(ui, |ui| self.top_view(ui));
            }
        }
    }
}

fn main() -> eframe::Result {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_title("spacetree"),
        ..Default::default()
    };
    eframe::run_native("spacetree", opts, Box::new(|_cc| Ok(Box::new(App::new()))))
}
