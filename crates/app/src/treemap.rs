//! Squarified treemap (Bruls, Huizing, van Wijk) over the arena tree.

use eframe::egui::{Pos2, Rect, pos2, vec2};
use spacetree_core::{NodeId, Tree};

/// Rectangles smaller than this (px²) are not laid out.
const MIN_AREA: f32 = 9.0;
/// Directories get nested layout only when both sides exceed this many px.
const MIN_NEST: f32 = 20.0;
const PAD: f32 = 2.0;

pub struct TmRect {
    pub id: NodeId,
    pub rect: Rect,
    /// Directory whose children are laid out inside (drawn as a frame, not filled).
    pub nested: bool,
}

/// Parent rectangles come before their children in the output.
pub fn layout(tree: &Tree, root: NodeId, area: Rect) -> Vec<TmRect> {
    let mut out = Vec::new();
    layout_dir(tree, root, area, &mut out);
    out
}

fn layout_dir(tree: &Tree, id: NodeId, rect: Rect, out: &mut Vec<TmRect>) {
    let total = tree.node(id).size as f64;
    if total <= 0.0 || rect.width() < 1.0 || rect.height() < 1.0 {
        return;
    }
    let scale = (rect.width() * rect.height()) as f64 / total;
    let items: Vec<(NodeId, f32)> = tree
        .children(id)
        .iter()
        .map(|&c| (c, (tree.node(c).size as f64 * scale) as f32))
        .take_while(|&(_, a)| a >= MIN_AREA)
        .collect();
    for (c, r) in squarify(&items, rect) {
        let nest = tree.is_dir(c) && r.width() > MIN_NEST && r.height() > MIN_NEST && tree.node(c).size > 0;
        out.push(TmRect {
            id: c,
            rect: r,
            nested: nest,
        });
        if nest {
            layout_dir(tree, c, r.shrink(PAD), out);
        }
    }
}

/// `items` must be sorted by area descending; areas sum to at most `r`'s area.
fn squarify(items: &[(NodeId, f32)], mut r: Rect) -> Vec<(NodeId, Rect)> {
    let mut out = Vec::with_capacity(items.len());
    let mut i = 0;
    while i < items.len() && r.width() > 0.5 && r.height() > 0.5 {
        let side = r.width().min(r.height());
        let (mut j, mut sum, mut best) = (i, 0.0f32, f32::INFINITY);
        while j < items.len() {
            let s = sum + items[j].1;
            let worst = worst_ratio(items[i].1, items[j].1, s, side);
            if worst > best {
                break;
            }
            best = worst;
            sum = s;
            j += 1;
        }
        let thick = sum / side;
        if r.width() >= r.height() {
            let mut y = r.min.y;
            for &(id, a) in &items[i..j] {
                let h = a / thick;
                out.push((id, Rect::from_min_size(pos2(r.min.x, y), vec2(thick, h))));
                y += h;
            }
            r.min.x += thick;
        } else {
            let mut x = r.min.x;
            for &(id, a) in &items[i..j] {
                let w = a / thick;
                out.push((id, Rect::from_min_size(pos2(x, r.min.y), vec2(w, thick))));
                x += w;
            }
            r.min.y += thick;
        }
        i = j;
    }
    out
}

/// Worst aspect ratio of a row with total area `sum` laid along `side`; `max`/`min` are its extreme areas.
fn worst_ratio(max: f32, min: f32, sum: f32, side: f32) -> f32 {
    let t = sum / side;
    ((max / t) / t).max(t / (min / t))
}

/// Deepest rectangle under `p` (children are emitted after their parents).
pub fn hit(rects: &[TmRect], p: Pos2) -> Option<&TmRect> {
    rects.iter().rev().find(|r| r.rect.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use spacetree_core::tree::{Builder, Kind, NO_PARENT};

    #[test]
    fn rects_stay_inside_and_do_not_overlap() {
        let mut b = Builder::default();
        let root = b.add(NO_PARENT, "r", Kind::Dir, 0, 0);
        for (i, s) in [500u64, 300, 200, 100, 50, 40, 10].iter().enumerate() {
            b.add(root, &format!("f{i}"), Kind::File, *s, *s);
        }
        let t = b.finish();
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 300.0));
        let rects = layout(&t, 0, area);
        assert_eq!(rects.len(), 7);
        let mut sum = 0.0;
        for (i, a) in rects.iter().enumerate() {
            assert!(area.expand(0.01).contains_rect(a.rect));
            sum += a.rect.area();
            for b in &rects[i + 1..] {
                assert!(!a.rect.shrink(0.01).intersects(b.rect.shrink(0.01)));
            }
        }
        assert!((sum - area.area()).abs() < 1.0, "areas must tile the parent: {sum}");
    }
}
