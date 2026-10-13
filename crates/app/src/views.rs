//! The four tabs: overview (tree / search results + treemap), file types, largest files, cleanup.

use eframe::egui::{
    self, Align2, Color32, FontId, Painter, Pos2, Rect, RichText, Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2,
};
use spacetree_core::cleanup::Safety;
use spacetree_core::stats::{extension, format_date, human_size};
use spacetree_core::{Kind, NodeId, ROOT, Tree};

use crate::theme::{self, ext_color, shade};
use crate::{Action, App, SortKey, Tab, TmCache, treemap};

const ROW_H: f32 = 22.0;
const INDENT: f32 = 16.0;
const CLEANUP_ITEMS: usize = 300;

fn font() -> FontId {
    FontId::proportional(13.0)
}

/// Right-aligned cell text ending at `right`.
fn cell(p: &Painter, right: f32, y: f32, text: impl ToString, color: Color32) {
    p.text(
        pos2(right - 8.0, y),
        Align2::RIGHT_CENTER,
        text.to_string(),
        font(),
        color,
    );
}

/// Shared right-click menu for any node.
fn node_menu(ui: &mut Ui, tree: &Tree, id: NodeId, act: &mut Option<Action>) {
    ui.label(RichText::new(tree.name(id)).strong());
    ui.separator();
    if ui.button("Показать в обзоре").clicked() {
        *act = Some(Action::Show(id));
        ui.close();
    }
    if ui.button("Показать в Проводнике").clicked() {
        *act = Some(Action::Reveal(id));
        ui.close();
    }
    if ui.button("Открыть").clicked() {
        *act = Some(Action::Open(id));
        ui.close();
    }
    if ui.button("Копировать путь").clicked() {
        *act = Some(Action::CopyPath(id));
        ui.close();
    }
}

/// Row background: selection, hover, zebra.
fn row_bg(ui: &Ui, rect: Rect, selected: bool, hovered: bool, i: usize) {
    let v = ui.visuals();
    let fill = if selected {
        v.selection.bg_fill
    } else if hovered {
        v.widgets.hovered.weak_bg_fill
    } else if i % 2 == 1 {
        v.faint_bg_color
    } else {
        return;
    };
    ui.painter().rect_filled(rect, 4.0, fill);
}

/// Small folder glyph centered at `c` (the emoji font has no good one).
fn folder_icon(p: &Painter, c: Pos2, color: Color32) {
    let body = Rect::from_center_size(c + vec2(0.0, 1.0), vec2(13.0, 9.0));
    let tab = Rect::from_min_size(body.left_top() - vec2(0.0, 2.0), vec2(5.5, 3.0));
    p.rect_filled(tab, 1.0, shade(color, 0.8));
    p.rect_filled(body, 1.5, color);
}

/// Expand/collapse triangle (pointing right when closed, down when open).
fn disclosure(p: &Painter, c: Pos2, open: bool, color: Color32) {
    let pts = if open {
        vec![c + vec2(-4.0, -2.5), c + vec2(4.0, -2.5), c + vec2(0.0, 3.0)]
    } else {
        vec![c + vec2(-2.5, -4.0), c + vec2(3.0, 0.0), c + vec2(-2.5, 4.0)]
    };
    p.add(Shape::convex_polygon(pts, color, Stroke::NONE));
}

/// Small up/down triangle centered at `c`.
fn sort_arrow(p: &Painter, c: Pos2, desc: bool, color: Color32) {
    let pts = if desc {
        vec![c + vec2(-4.0, -2.0), c + vec2(4.0, -2.0), c + vec2(0.0, 3.0)]
    } else {
        vec![c + vec2(-4.0, 2.0), c + vec2(4.0, 2.0), c + vec2(0.0, -3.0)]
    };
    p.add(Shape::convex_polygon(pts, color, Stroke::NONE));
}

/// File-type marker: folder glyph, link glyph or a colored square.
fn kind_icon(p: &Painter, tree: &Tree, id: NodeId, at: Pos2, text: Color32) {
    match tree.node(id).kind {
        Kind::Dir => folder_icon(p, at + vec2(7.0, 0.0), theme::LIKELY),
        Kind::Link => {
            p.text(at, Align2::LEFT_CENTER, "↪", FontId::proportional(13.0), text);
        }
        Kind::File => {
            let r = Rect::from_center_size(at + vec2(6.0, 0.0), vec2(9.0, 9.0));
            p.rect_filled(r, 2.0, ext_color(&extension(tree.name(id))));
        }
    }
}

impl App {
    fn sort_header(&mut self, p: &Painter, resp: &egui::Response, rect: Rect, key: SortKey, label: &str, right: bool) {
        let active = self.sort == key;
        let weak = Color32::from_gray(140);
        let color = if active { theme::ACCENT } else { weak };
        let y = rect.center().y;
        let text = if right {
            p.text(pos2(rect.right() - 8.0, y), Align2::RIGHT_CENTER, label, font(), color)
        } else {
            p.text(pos2(rect.left() + 8.0, y), Align2::LEFT_CENTER, label, font(), color)
        };
        if active {
            let x = if right { text.left() - 9.0 } else { text.right() + 9.0 };
            sort_arrow(p, pos2(x, y), self.sort_desc, color);
        }
        if resp.clicked()
            && let Some(pos) = resp.interact_pointer_pos()
            && rect.contains(pos)
        {
            if active {
                self.sort_desc = !self.sort_desc;
            } else {
                self.sort = key;
                self.sort_desc = key != SortKey::Name;
            }
            self.rows_dirty = true;
        }
    }

    pub fn tree_view(&mut self, ui: &mut Ui) {
        let Some(data) = self.data.clone() else { return };
        let tree = &data.out.tree;
        if self.rows_dirty {
            self.rebuild_rows();
        }
        let width = ui.available_width();
        let (w_size, w_pct, w_files, w_date) = (88.0, 120.0, 72.0, 92.0);
        let cols_w = w_size * 2.0 + w_pct + w_files + w_date;
        let weak = ui.visuals().weak_text_color();
        let text_col = ui.visuals().text_color();

        // header
        let (hdr, hresp) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::click());
        let p = ui.painter_at(hdr);
        let mut x = hdr.right();
        let mut col = |w: f32| {
            let r = Rect::from_min_max(pos2(x - w, hdr.top()), pos2(x, hdr.bottom()));
            x -= w;
            r
        };
        let (r_date, r_files, r_pct, r_alloc, r_size) =
            (col(w_date), col(w_files), col(w_pct), col(w_size), col(w_size));
        let r_name = Rect::from_min_max(hdr.min, pos2(r_size.left(), hdr.bottom()));
        self.sort_header(&p, &hresp, r_name, SortKey::Name, "Имя", false);
        self.sort_header(&p, &hresp, r_size, SortKey::Size, "Размер", true);
        p.text(
            pos2(r_alloc.right() - 8.0, hdr.center().y),
            Align2::RIGHT_CENTER,
            "На диске",
            font(),
            weak,
        );
        p.text(
            pos2(r_pct.right() - 8.0, hdr.center().y),
            Align2::RIGHT_CENTER,
            "% папки",
            font(),
            weak,
        );
        self.sort_header(&p, &hresp, r_files, SortKey::Files, "Файлов", true);
        self.sort_header(&p, &hresp, r_date, SortKey::Modified, "Изменён", true);
        ui.painter().hline(
            hdr.x_range(),
            hdr.bottom(),
            ui.visuals().widgets.noninteractive.bg_stroke,
        );

        let mut sa = egui::ScrollArea::vertical().auto_shrink([false, false]);
        if let Some(r) = self.scroll_to.take() {
            sa = sa.vertical_scroll_offset((r as f32 * (ROW_H + ui.spacing().item_spacing.y) - 80.0).max(0.0));
        }
        let mut act: Option<Action> = None;
        let selected = self.selected;
        let rows = &self.rows;
        let expanded = &self.expanded;
        sa.show_rows(ui, ROW_H, rows.len(), |ui, range| {
            for i in range {
                let (id, depth) = rows[i];
                let (rect, resp) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::click());
                row_bg(ui, rect, selected == Some(id), resp.hovered(), i);
                let painter = ui.painter_at(rect);
                let nd = tree.node(id);
                let is_dir = nd.kind == Kind::Dir;
                let tx = rect.left() + 4.0 + depth as f32 * INDENT;
                if is_dir && !tree.children(id).is_empty() {
                    disclosure(&painter, pos2(tx + 6.0, rect.center().y), expanded.contains(&id), weak);
                }
                let name_clip = Rect::from_min_max(rect.min, pos2(rect.right() - cols_w, rect.bottom()));
                let np = ui.painter_at(name_clip);
                kind_icon(&np, tree, id, pos2(tx + 15.0, rect.center().y), text_col);
                np.text(
                    pos2(tx + 34.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    tree.name(id),
                    font(),
                    if nd.kind == Kind::Link { weak } else { text_col },
                );
                let parent_size = tree.parent(id).map(|p| tree.node(p).size).unwrap_or(nd.size);
                let frac = if parent_size == 0 {
                    0.0
                } else {
                    nd.size as f64 / parent_size as f64
                };
                let y = rect.center().y;
                let mut x = rect.right();
                cell(&painter, x, y, format_date(nd.mtime), weak);
                x -= w_date;
                cell(
                    &painter,
                    x,
                    y,
                    if is_dir { nd.files.to_string() } else { String::new() },
                    weak,
                );
                x -= w_files;
                let bar = Rect::from_min_size(pos2(x - w_pct + 6.0, y - 4.0), vec2(56.0, 8.0));
                theme::bar(ui, bar, frac as f32, theme::ACCENT.gamma_multiply(0.85));
                cell(&painter, x, y, format!("{:.1}%", frac * 100.0), text_col);
                x -= w_pct;
                cell(&painter, x, y, human_size(nd.alloc), weak);
                x -= w_size;
                cell(&painter, x, y, human_size(nd.size), text_col);

                if resp.clicked() {
                    let on_arrow = resp.interact_pointer_pos().is_some_and(|p| p.x < tx + 14.0);
                    act = Some(if is_dir && on_arrow {
                        Action::Toggle(id)
                    } else {
                        Action::Show(id)
                    });
                }
                if resp.double_clicked() && is_dir {
                    act = Some(Action::Toggle(id));
                }
                resp.context_menu(|ui| node_menu(ui, tree, id, &mut act));
            }
        });
        if let Some(a) = act {
            // a click in the tree keeps the tab; Show only re-targets the treemap
            self.apply(ui.ctx(), a);
        }
    }

    pub fn search_view(&mut self, ui: &mut Ui) {
        let Some(data) = self.data.clone() else { return };
        let tree = &data.out.tree;
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Найдено: {}", self.search_hits.len())).strong());
            if self.search_truncated {
                ui.label(RichText::new("(показаны крупнейшие)").weak());
            }
            if ui.small_button("× Сбросить поиск").clicked() {
                self.search.clear();
            }
        });
        ui.separator();
        let width = ui.available_width();
        let weak = ui.visuals().weak_text_color();
        let text_col = ui.visuals().text_color();
        let mut act = None;
        let selected = self.selected;
        let hits = &self.search_hits;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, ROW_H * 2.0, hits.len(), |ui, range| {
                for i in range {
                    let id = hits[i];
                    let (rect, resp) = ui.allocate_exact_size(vec2(width, ROW_H * 2.0), Sense::click());
                    row_bg(ui, rect, selected == Some(id), resp.hovered(), i);
                    let p = ui.painter_at(rect.shrink2(vec2(0.0, 1.0)));
                    let top = rect.top() + ROW_H * 0.55;
                    kind_icon(&p, tree, id, pos2(rect.left() + 8.0, top), text_col);
                    p.text(
                        pos2(rect.left() + 28.0, top),
                        Align2::LEFT_CENTER,
                        tree.name(id),
                        font(),
                        text_col,
                    );
                    cell(&p, rect.right(), top, human_size(tree.node(id).size), text_col);
                    let parent = tree.parent(id).map(|p| tree.path(p)).unwrap_or_default();
                    p.text(
                        pos2(rect.left() + 28.0, rect.top() + ROW_H * 1.45),
                        Align2::LEFT_CENTER,
                        parent,
                        FontId::proportional(11.5),
                        weak,
                    );
                    if resp.clicked() {
                        act = Some(Action::Select(id));
                        self.tm_root = if tree.is_dir(id) {
                            id
                        } else {
                            tree.parent(id).unwrap_or(ROOT)
                        };
                    }
                    resp.context_menu(|ui| node_menu(ui, tree, id, &mut act));
                }
            });
        if let Some(a) = act {
            self.apply(ui.ctx(), a);
        }
    }

    pub fn treemap_view(&mut self, ui: &mut Ui) {
        let Some(data) = self.data.clone() else { return };
        let tree = &data.out.tree;
        let mut goto: Option<NodeId> = None;
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(self.tm_root != ROOT, egui::Button::new("⬆"))
                .on_hover_text("На уровень выше (Backspace)")
                .clicked()
            {
                self.tm_up();
            }
            for (i, &c) in tree.ancestors(self.tm_root).iter().enumerate() {
                if i > 0 {
                    ui.label(RichText::new("›").weak());
                }
                let name = if c == ROOT {
                    tree.path(ROOT)
                } else {
                    tree.name(c).to_owned()
                };
                if ui.link(name).clicked() {
                    goto = Some(c);
                }
            }
            ui.label(RichText::new(human_size(tree.node(self.tm_root).size)).weak());
        });
        if let Some(c) = goto {
            self.tm_root = c;
        }

        // legend: largest types, click to highlight
        let legend_h = 26.0;
        let avail = ui.available_size();
        let (resp, painter) = ui.allocate_painter(vec2(avail.x, (avail.y - legend_h).max(50.0)), Sense::click());
        let area = resp.rect;
        painter.rect_filled(area, 6.0, ui.visuals().extreme_bg_color);
        if self
            .tm
            .as_ref()
            .is_none_or(|c| c.root != self.tm_root || c.size != area.size())
        {
            let rects = treemap::layout(tree, self.tm_root, Rect::from_min_size(Pos2::ZERO, area.size()));
            let exts: Vec<String> = rects
                .iter()
                .map(|r| {
                    if tree.is_dir(r.id) {
                        String::new()
                    } else {
                        extension(tree.name(r.id))
                    }
                })
                .collect();
            let colors = rects
                .iter()
                .zip(&exts)
                .map(|(r, e)| {
                    if tree.is_dir(r.id) {
                        theme::DIR_TILE
                    } else {
                        ext_color(e)
                    }
                })
                .collect();
            self.tm = Some(TmCache {
                root: self.tm_root,
                size: area.size(),
                rects,
                colors,
                exts,
            });
        }
        let cache = self.tm.take().unwrap();
        let off = area.min.to_vec2();
        let hover = resp
            .hover_pos()
            .and_then(|p| treemap::hit(&cache.rects, p - off))
            .map(|r| r.id);

        // All leaf tiles go into one mesh with a light-to-dark diagonal gradient.
        let mut mesh = egui::Mesh::default();
        let mut labels: Vec<(Rect, NodeId, bool)> = Vec::new();
        let frame = Color32::from_gray(if ui.visuals().dark_mode { 70 } else { 150 });
        for (k, r) in cache.rects.iter().enumerate() {
            let rect = r.rect.translate(off);
            if r.nested {
                painter.rect_stroke(rect, 0.0, Stroke::new(1.0, frame), StrokeKind::Inside);
                if r.header {
                    labels.push((rect, r.id, true));
                }
                continue;
            }
            let dim = self.highlight_ext.as_ref().is_some_and(|h| *h != cache.exts[k]);
            let base = if dim {
                shade(cache.colors[k], 0.3)
            } else {
                cache.colors[k]
            };
            let t = rect.shrink(0.5);
            let (c0, c1, c2) = (shade(base, 1.18), shade(base, 0.72), shade(base, 0.95));
            let i = mesh.vertices.len() as u32;
            mesh.colored_vertex(t.left_top(), c0);
            mesh.colored_vertex(t.right_top(), c2);
            mesh.colored_vertex(t.right_bottom(), c1);
            mesh.colored_vertex(t.left_bottom(), c2);
            mesh.add_triangle(i, i + 1, i + 2);
            mesh.add_triangle(i, i + 2, i + 3);
            if rect.width() > 60.0 && rect.height() > 16.0 && !dim {
                labels.push((rect, r.id, false));
            }
        }
        painter.add(Shape::mesh(mesh));
        let folder_text = if ui.visuals().dark_mode {
            Color32::from_gray(210)
        } else {
            Color32::from_gray(40)
        };
        for (rect, id, folder) in labels {
            let (pos, color) = if folder {
                (rect.min + vec2(3.0, 1.0), folder_text)
            } else {
                (rect.min + vec2(3.0, 2.0), Color32::BLACK)
            };
            painter.with_clip_rect(rect.intersect(area)).text(
                pos,
                Align2::LEFT_TOP,
                tree.name(id),
                FontId::proportional(11.0),
                color,
            );
        }
        for (id, c) in [(self.selected, Color32::WHITE), (hover, Color32::YELLOW)] {
            if let Some(id) = id
                && let Some(r) = cache.rects.iter().find(|r| r.id == id)
            {
                painter.rect_stroke(r.rect.translate(off), 0.0, Stroke::new(2.0, c), StrokeKind::Inside);
            }
        }
        if cache.rects.is_empty() {
            painter.text(
                area.center(),
                Align2::CENTER_CENTER,
                "Папка пуста",
                font(),
                ui.visuals().weak_text_color(),
            );
        }

        let root_size = tree.node(self.tm_root).size.max(1);
        if let Some(h) = hover {
            let nd = tree.node(h);
            resp.clone().on_hover_text(format!(
                "{}\n{} · на диске {} · {:.1}% от текущей папки{}",
                tree.path(h),
                human_size(nd.size),
                human_size(nd.alloc),
                nd.size as f64 * 100.0 / root_size as f64,
                if nd.mtime != 0 {
                    format!("\nизменён {}", format_date(nd.mtime))
                } else {
                    String::new()
                }
            ));
        }
        self.tm = Some(cache);

        let mut act = None;
        if resp.secondary_clicked() {
            self.tm_menu = hover;
        }
        if let Some(id) = self.tm_menu {
            resp.context_menu(|ui| node_menu(ui, tree, id, &mut act));
        }
        // click selects, double click drills into a folder; Backspace / mouse "back" goes up
        if let Some(h) = hover {
            if resp.double_clicked() && tree.is_dir(h) {
                self.select(h);
            } else if resp.clicked() {
                act = Some(Action::Select(h));
            }
        }
        let back =
            ui.input(|i| i.pointer.button_clicked(egui::PointerButton::Extra1) || i.key_pressed(egui::Key::Backspace));
        if back && resp.hovered() {
            self.tm_up();
        }
        if let Some(a) = act {
            self.apply(ui.ctx(), a);
        }

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            for t in data.types.iter().take(10) {
                let on = self.highlight_ext.as_deref() == Some(t.ext.as_str());
                let name = if t.ext.is_empty() {
                    "без расширения".to_owned()
                } else {
                    format!(".{}", t.ext)
                };
                let (r, resp) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::click());
                ui.painter().rect_filled(r, 3.0, ext_color(&t.ext));
                let label = ui.add(
                    egui::Label::new(RichText::new(name).color(if on {
                        theme::ACCENT
                    } else {
                        ui.visuals().text_color()
                    }))
                    .sense(Sense::click()),
                );
                if resp.clicked() || label.clicked() {
                    self.highlight_ext = if on { None } else { Some(t.ext.clone()) };
                }
                label.on_hover_text(format!(
                    "{} · {} файлов. Нажмите, чтобы подсветить",
                    human_size(t.size),
                    t.count
                ));
            }
            if self.highlight_ext.is_some() && ui.small_button("×").on_hover_text("Снять подсветку").clicked()
            {
                self.highlight_ext = None;
            }
        });
    }

    pub fn types_view(&mut self, ui: &mut Ui) {
        let Some(data) = self.data.clone() else { return };
        let total = data.out.tree.root_stats().0.max(1) as f64;
        let max = data.types.first().map_or(1, |t| t.size.max(1)) as f32;
        let types = &data.types;
        ui.label(
            RichText::new("Нажмите на тип, чтобы подсветить его файлы в treemap; двойной щелчок — перейти к обзору.")
                .weak(),
        );
        ui.add_space(4.0);
        let width = ui.available_width();
        let weak = ui.visuals().weak_text_color();
        let text_col = ui.visuals().text_color();
        let (hdr, _) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::hover());
        let p = ui.painter_at(hdr);
        let cols = [(90.0, "Файлов"), (70.0, "%"), (100.0, "На диске"), (100.0, "Размер")];
        let mut x = hdr.right();
        for (w, t) in cols {
            cell(&p, x, hdr.center().y, t, weak);
            x -= w;
        }
        p.text(
            pos2(hdr.left() + 30.0, hdr.center().y),
            Align2::LEFT_CENTER,
            "Расширение",
            font(),
            weak,
        );
        let mut pick = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, ROW_H, types.len(), |ui, range| {
                for i in range {
                    let t = &types[i];
                    let on = self.highlight_ext.as_deref() == Some(t.ext.as_str());
                    let (rect, resp) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::click());
                    row_bg(ui, rect, on, resp.hovered(), i);
                    let p = ui.painter_at(rect);
                    let y = rect.center().y;
                    p.rect_filled(
                        Rect::from_center_size(pos2(rect.left() + 14.0, y), vec2(11.0, 11.0)),
                        3.0,
                        ext_color(&t.ext),
                    );
                    let name = if t.ext.is_empty() {
                        "(без расширения)".to_owned()
                    } else {
                        format!(".{}", t.ext)
                    };
                    p.text(pos2(rect.left() + 30.0, y), Align2::LEFT_CENTER, name, font(), text_col);
                    let mut x = rect.right();
                    cell(&p, x, y, t.count, weak);
                    x -= 90.0;
                    cell(&p, x, y, format!("{:.1}%", t.size as f64 * 100.0 / total), text_col);
                    x -= 70.0;
                    cell(&p, x, y, human_size(t.alloc), weak);
                    x -= 100.0;
                    cell(&p, x, y, human_size(t.size), text_col);
                    x -= 100.0;
                    let bar_w = (x - rect.left() - 200.0).clamp(0.0, 260.0);
                    if bar_w > 20.0 {
                        let r = Rect::from_min_size(pos2(x - bar_w - 8.0, y - 4.0), vec2(bar_w, 8.0));
                        theme::bar(ui, r, t.size as f32 / max, ext_color(&t.ext));
                    }
                    if resp.double_clicked() {
                        pick = Some((Some(t.ext.clone()), true));
                    } else if resp.clicked() {
                        pick = Some((if on { None } else { Some(t.ext.clone()) }, false));
                    }
                }
            });
        if let Some((ext, go)) = pick {
            self.highlight_ext = ext;
            if go {
                self.tab = Tab::Overview;
            }
        }
    }

    pub fn top_view(&mut self, ui: &mut Ui) {
        let Some(data) = self.data.clone() else { return };
        let tree = &data.out.tree;
        let width = ui.available_width();
        let weak = ui.visuals().weak_text_color();
        let text_col = ui.visuals().text_color();
        let (hdr, _) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::hover());
        let p = ui.painter_at(hdr);
        cell(&p, hdr.left() + 100.0, hdr.center().y, "Размер", weak);
        cell(&p, hdr.left() + 200.0, hdr.center().y, "Изменён", weak);
        p.text(
            pos2(hdr.left() + 216.0, hdr.center().y),
            Align2::LEFT_CENTER,
            "Путь",
            font(),
            weak,
        );
        let mut act = None;
        let selected = self.selected;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, ROW_H, data.top.len(), |ui, range| {
                for i in range {
                    let id = data.top[i];
                    let nd = tree.node(id);
                    let (rect, resp) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::click());
                    row_bg(ui, rect, selected == Some(id), resp.hovered(), i);
                    let p = ui.painter_at(rect);
                    let y = rect.center().y;
                    cell(&p, rect.left() + 100.0, y, human_size(nd.size), text_col);
                    cell(&p, rect.left() + 200.0, y, format_date(nd.mtime), weak);
                    kind_icon(&p, tree, id, pos2(rect.left() + 210.0, y), text_col);
                    p.text(
                        pos2(rect.left() + 230.0, y),
                        Align2::LEFT_CENTER,
                        tree.path(id),
                        font(),
                        text_col,
                    );
                    if resp.clicked() {
                        act = Some(Action::Show(id));
                    }
                    resp.on_hover_text("Нажмите, чтобы показать в обзоре; правая кнопка — меню")
                        .context_menu(|ui| node_menu(ui, tree, id, &mut act));
                }
            });
        if let Some(a) = act {
            self.apply(ui.ctx(), a);
        }
    }

    pub fn cleanup_view(&mut self, ui: &mut Ui) {
        let Some(data) = self.data.clone() else { return };
        let tree = &data.out.tree;
        let levels = [Safety::Safe, Safety::Likely, Safety::Review];
        let sum = |s: Safety| -> u64 {
            data.cleanup
                .iter()
                .filter(|g| g.category.safety() == s)
                .map(|g| g.size)
                .sum()
        };

        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("≈ {}", human_size(data.cleanup_total)))
                        .size(26.0)
                        .strong(),
                );
                ui.label(RichText::new("можно освободить").size(16.0).weak());
            });
            ui.label(
                RichText::new(
                    "Это подсказки по именам и расположению файлов. spacetree ничего не удаляет сам: \
                     проверьте и удалите нужное через Проводник (правая кнопка › «Показать в Проводнике»). \
                     Закройте программы, чей кэш чистите.",
                )
                .weak(),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Показывать:");
                for (k, s) in levels.iter().enumerate() {
                    let c = theme::safety_color(*s);
                    let text = RichText::new(format!("{} — {}", s.label(), human_size(sum(*s)))).color(
                        if self.cleanup_show[k] {
                            c
                        } else {
                            ui.visuals().weak_text_color()
                        },
                    );
                    if ui.add(egui::Button::selectable(self.cleanup_show[k], text)).clicked() {
                        self.cleanup_show[k] = !self.cleanup_show[k];
                    }
                }
            });
        });
        ui.add_space(8.0);
        if data.cleanup.is_empty() {
            ui.label("Ничего подходящего не найдено.");
            return;
        }

        let mut act = None;
        let selected = self.selected;
        let weak = ui.visuals().weak_text_color();
        let text_col = ui.visuals().text_color();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (gi, g) in data.cleanup.iter().enumerate() {
                let s = g.category.safety();
                if !self.cleanup_show[levels.iter().position(|l| *l == s).unwrap()] {
                    continue;
                }
                let open = self.cleanup_open.contains(&gi);
                let header = theme::card(ui)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                            disclosure(ui.painter(), r.center(), open, ui.visuals().weak_text_color());
                            theme::badge(ui, s.label(), theme::safety_color(s));
                            ui.label(RichText::new(g.category.title()).strong().size(14.5));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(RichText::new(human_size(g.size)).strong().size(14.5));
                                ui.label(
                                    RichText::new(format!("{} объектов · {} файлов", g.items.len(), g.files)).weak(),
                                );
                            });
                        });
                        ui.label(RichText::new(g.category.hint()).weak());
                        if open {
                            ui.add_space(4.0);
                            let width = ui.available_width();
                            for (i, &id) in g.items.iter().take(CLEANUP_ITEMS).enumerate() {
                                let nd = tree.node(id);
                                let (rect, resp) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::click());
                                row_bg(ui, rect, selected == Some(id), resp.hovered(), i);
                                let p = ui.painter_at(rect);
                                let y = rect.center().y;
                                cell(&p, rect.left() + 90.0, y, human_size(nd.size), text_col);
                                cell(&p, rect.left() + 180.0, y, format_date(nd.mtime), weak);
                                kind_icon(&p, tree, id, pos2(rect.left() + 190.0, y), text_col);
                                let path_clip = Rect::from_min_max(rect.min, pos2(rect.right() - 84.0, rect.bottom()));
                                ui.painter_at(path_clip).text(
                                    pos2(rect.left() + 210.0, y),
                                    Align2::LEFT_CENTER,
                                    tree.path(id),
                                    font(),
                                    text_col,
                                );
                                let btn = Rect::from_center_size(pos2(rect.right() - 40.0, y), vec2(76.0, 20.0));
                                let b = ui
                                    .put(
                                        btn,
                                        egui::Button::new(RichText::new("В Проводнике").size(11.5))
                                            .frame(resp.hovered()),
                                    )
                                    .on_hover_text("Показать в Проводнике");
                                if b.clicked() {
                                    act = Some(Action::Reveal(id));
                                } else if resp.double_clicked() {
                                    act = Some(Action::Show(id));
                                } else if resp.clicked() {
                                    act = Some(Action::Select(id));
                                }
                                resp.context_menu(|ui| node_menu(ui, tree, id, &mut act));
                            }
                            if g.items.len() > CLEANUP_ITEMS {
                                ui.label(
                                    RichText::new(format!(
                                        "…и ещё {} (показаны крупнейшие)",
                                        g.items.len() - CLEANUP_ITEMS
                                    ))
                                    .weak(),
                                );
                            }
                        }
                    })
                    .response;
                // the header area (above the item list) toggles the group
                let head = Rect::from_min_size(header.rect.min, vec2(header.rect.width(), 52.0));
                if ui
                    .interact(head, egui::Id::new(("cleanup", gi)), Sense::click())
                    .clicked()
                {
                    if open {
                        self.cleanup_open.remove(&gi);
                    } else {
                        self.cleanup_open.insert(gi);
                    }
                }
                ui.add_space(6.0);
            }
        });
        if let Some(a) = act {
            self.apply(ui.ctx(), a);
        }
    }
}
