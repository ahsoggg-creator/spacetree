//! Look & feel: palette, egui style for both themes, small painting helpers.

use eframe::egui::{
    self, Color32, CornerRadius, FontId, Margin, Rect, Response, RichText, Sense, Stroke, Theme, Ui, Vec2, vec2,
};
use spacetree_core::cleanup::Safety;

pub const ACCENT: Color32 = Color32::from_rgb(76, 141, 255);
pub const SAFE: Color32 = Color32::from_rgb(63, 185, 80);
pub const LIKELY: Color32 = Color32::from_rgb(210, 153, 34);
pub const REVIEW: Color32 = Color32::from_rgb(219, 109, 40);
pub const DIR_TILE: Color32 = Color32::from_rgb(70, 80, 100);

pub fn safety_color(s: Safety) -> Color32 {
    match s {
        Safety::Safe => SAFE,
        Safety::Likely => LIKELY,
        Safety::Review => REVIEW,
    }
}

pub fn install(ctx: &egui::Context) {
    for theme in [Theme::Dark, Theme::Light] {
        ctx.style_mut_of(theme, |s| {
            let dark = theme == Theme::Dark;
            s.spacing.item_spacing = vec2(8.0, 6.0);
            s.spacing.button_padding = vec2(10.0, 4.0);
            s.spacing.menu_margin = Margin::same(6);
            s.spacing.interact_size.y = 24.0;
            let v = &mut s.visuals;
            let r = CornerRadius::same(6);
            v.window_corner_radius = CornerRadius::same(10);
            v.menu_corner_radius = CornerRadius::same(8);
            for w in [
                &mut v.widgets.noninteractive,
                &mut v.widgets.inactive,
                &mut v.widgets.hovered,
                &mut v.widgets.active,
                &mut v.widgets.open,
            ] {
                w.corner_radius = r;
            }
            v.selection.bg_fill = if dark {
                Color32::from_rgb(38, 79, 150)
            } else {
                Color32::from_rgb(190, 214, 255)
            };
            v.selection.stroke = Stroke::new(1.0, ACCENT);
            v.hyperlink_color = ACCENT;
            if dark {
                v.panel_fill = Color32::from_rgb(30, 31, 34);
                v.window_fill = Color32::from_rgb(36, 37, 41);
                v.extreme_bg_color = Color32::from_rgb(20, 21, 23);
                v.faint_bg_color = Color32::from_rgb(36, 37, 41);
                v.widgets.inactive.weak_bg_fill = Color32::from_rgb(48, 50, 55);
                v.widgets.inactive.bg_fill = Color32::from_rgb(48, 50, 55);
                v.widgets.hovered.weak_bg_fill = Color32::from_rgb(60, 63, 70);
                v.widgets.hovered.bg_fill = Color32::from_rgb(60, 63, 70);
            } else {
                v.panel_fill = Color32::from_rgb(246, 247, 249);
                v.window_fill = Color32::WHITE;
                v.extreme_bg_color = Color32::from_rgb(232, 234, 238);
                v.faint_bg_color = Color32::from_rgb(238, 240, 243);
            }
        });
    }
}

/// Scales the RGB channels of a color (`f` > 1 lightens, < 1 darkens).
pub fn shade(c: Color32, f: f32) -> Color32 {
    let m = |v: u8| (v as f32 * f).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(m(c.r()), m(c.g()), m(c.b()))
}

/// Stable pleasant color for a file extension.
pub fn ext_color(ext: &str) -> Color32 {
    if ext.is_empty() {
        return Color32::from_gray(140);
    }
    let mut h: u32 = 2166136261;
    for b in ext.bytes() {
        h = (h ^ b as u32).wrapping_mul(16777619);
    }
    let hue = (h % 360) as f32 / 360.0;
    egui::ecolor::Hsva::new(hue, 0.55, 0.80, 1.0).into()
}

/// Horizontal bar showing `frac` (0..=1) inside `rect`.
pub fn bar(ui: &Ui, rect: Rect, frac: f32, color: Color32) {
    let p = ui.painter();
    p.rect_filled(rect, 3.0, ui.visuals().extreme_bg_color);
    if frac > 0.0 {
        let mut f = rect;
        f.set_width((rect.width() * frac.clamp(0.0, 1.0)).max(2.0));
        p.rect_filled(f, 3.0, color);
    }
}

/// Small rounded label with a colored background.
pub fn badge(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let font = FontId::proportional(11.5);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, Color32::WHITE);
    let size = galley.size() + vec2(12.0, 4.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(rect, 9.0, color.gamma_multiply(0.85));
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, Color32::WHITE);
    resp
}

/// Primary (accent-filled) button.
pub fn primary_button(ui: &mut Ui, text: &str) -> Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(Color32::WHITE).strong())
            .fill(ACCENT)
            .min_size(Vec2::new(0.0, 26.0)),
    )
}

/// Card-like frame for grouped content.
pub fn card(ui: &Ui) -> egui::Frame {
    egui::Frame::new()
        .fill(ui.visuals().window_fill)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .corner_radius(10)
        .inner_margin(Margin::same(12))
}

/// Linear blend between two colors (`t` in 0..=1).
pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()), m(a.a(), b.a()))
}

/// Treemap color by modification age: fresh = warm, old = cold.
pub fn age_color(mtime: u32, now: u32) -> Color32 {
    if mtime == 0 {
        return Color32::from_gray(120);
    }
    let days = now.saturating_sub(mtime) as f32 / 86_400.0;
    const STOPS: [(f32, Color32); 6] = [
        (0.0, Color32::from_rgb(240, 90, 80)),
        (7.0, Color32::from_rgb(245, 160, 70)),
        (30.0, Color32::from_rgb(230, 210, 90)),
        (180.0, Color32::from_rgb(110, 200, 120)),
        (365.0, Color32::from_rgb(80, 160, 230)),
        (1095.0, Color32::from_rgb(140, 110, 220)),
    ];
    for w in STOPS.windows(2) {
        if days < w[1].0 {
            return lerp_color(w[0].1, w[1].1, (days - w[0].0) / (w[1].0 - w[0].0));
        }
    }
    STOPS[STOPS.len() - 1].1
}

/// Legend entries for [`age_color`].
pub const AGE_LEGEND: [(&str, u32); 6] = [
    ("сегодня", 0),
    ("неделя", 7),
    ("месяц", 30),
    ("полгода", 180),
    ("год", 365),
    ("3+ года", 1095),
];

/// Indeterminate progress: an accent segment sliding along a track.
pub fn marquee(ui: &mut Ui, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 8.0), Sense::hover());
    ui.painter().rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
    let t = (ui.input(|i| i.time) * 0.8).fract() as f32;
    let seg = width * 0.3;
    let x = rect.left() - seg + (width + seg) * t;
    let r = Rect::from_min_max(
        egui::pos2(x.max(rect.left()), rect.top()),
        egui::pos2((x + seg).min(rect.right()), rect.bottom()),
    );
    if r.width() > 0.0 {
        ui.painter().rect_filled(r, 4.0, ACCENT);
    }
    ui.ctx().request_repaint();
}
