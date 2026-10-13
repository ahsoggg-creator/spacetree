use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use rayon::prelude::*;

use crate::tree::{Kind, NodeId, ROOT, Tree};

/// Lower-cased extension without the dot; empty string when there is none.
pub fn extension(name: &str) -> String {
    ext_raw(name).to_lowercase()
}

/// Extension as written in the name (not lower-cased); empty when there is none.
pub fn ext_raw(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => &name[i + 1..],
        _ => "",
    }
}

/// Largest `n` files, biggest first.
pub fn top_files(tree: &Tree, n: usize) -> Vec<NodeId> {
    type Heap = BinaryHeap<Reverse<(u64, NodeId)>>;
    if n == 0 {
        return Vec::new();
    }
    let offer = |heap: &mut Heap, item: Reverse<(u64, NodeId)>| {
        if heap.len() < n {
            heap.push(item);
        } else if let Some(min) = heap.peek()
            && item < *min
        {
            heap.pop();
            heap.push(item);
        }
    };
    let heap = tree
        .nodes()
        .par_iter()
        .enumerate()
        .with_min_len(16384)
        .fold(Heap::new, |mut h, (i, nd)| {
            if nd.kind == Kind::File && !nd.removed {
                offer(&mut h, Reverse((nd.size, i as NodeId)));
            }
            h
        })
        .reduce(Heap::new, |mut a, b| {
            for item in b {
                offer(&mut a, item);
            }
            a
        });
    let mut v: Vec<(u64, NodeId)> = heap.into_iter().map(|Reverse(x)| x).collect();
    v.sort_unstable_by(|a, b| b.cmp(a));
    v.into_iter().map(|(_, id)| id).collect()
}

#[derive(Clone, Debug)]
pub struct TypeStat {
    /// Empty = files without extension.
    pub ext: String,
    pub size: u64,
    pub alloc: u64,
    pub count: u64,
}

type TypeMap = HashMap<String, TypeStat>;

fn add_type(map: &mut TypeMap, name: &str, size: u64, alloc: u64) {
    let raw = ext_raw(name);
    let lower;
    // Most extensions are already lower-case ASCII: look them up without allocating.
    let key = if raw.bytes().any(|b| b.is_ascii_uppercase() || !b.is_ascii()) {
        lower = raw.to_lowercase();
        lower.as_str()
    } else {
        raw
    };
    let e = match map.get_mut(key) {
        Some(e) => e,
        None => map.entry(key.to_owned()).or_insert(TypeStat {
            ext: key.to_owned(),
            size: 0,
            alloc: 0,
            count: 0,
        }),
    };
    e.size += size;
    e.alloc += alloc;
    e.count += 1;
}

fn merge_types(mut a: TypeMap, b: TypeMap) -> TypeMap {
    for (k, v) in b {
        let e = a.entry(k).or_insert(TypeStat {
            ext: v.ext.clone(),
            size: 0,
            alloc: 0,
            count: 0,
        });
        e.size += v.size;
        e.alloc += v.alloc;
        e.count += v.count;
    }
    a
}

/// Per-extension totals over the files of the subtree below `root`, sorted by size descending.
pub fn type_stats(tree: &Tree, root: NodeId) -> Vec<TypeStat> {
    let map = if root == ROOT {
        // Whole tree: every file belongs to it, so scan the node array in parallel.
        tree.nodes()
            .par_iter()
            .enumerate()
            .with_min_len(4096)
            .fold(TypeMap::new, |mut m, (i, nd)| {
                if nd.kind == Kind::File && !nd.removed {
                    add_type(&mut m, tree.name(i as NodeId), nd.size, nd.alloc);
                }
                m
            })
            .reduce(TypeMap::new, merge_types)
    } else {
        let mut map = TypeMap::new();
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            let nd = tree.node(id);
            match nd.kind {
                Kind::Dir => stack.extend_from_slice(tree.children(id)),
                Kind::File => add_type(&mut map, tree.name(id), nd.size, nd.alloc),
                Kind::Link => {}
            }
        }
        map
    };
    let mut v: Vec<TypeStat> = map.into_values().collect();
    v.sort_unstable_by(|a, b| b.size.cmp(&a.size).then_with(|| a.ext.cmp(&b.ext)));
    v
}

/// Unix seconds -> "дд.мм.гггг" (UTC); empty for 0 (unknown).
pub fn format_date(unix: u32) -> String {
    if unix == 0 {
        return String::new();
    }
    // Days -> civil date, H. Hinnant's algorithm (http://howardhinnant.github.io/date_algorithms.html).
    let z = (unix / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{d:02}.{m:02}.{y}")
}

/// 1536 -> "1.50 КБ".
pub fn human_size(n: u64) -> String {
    const U: [&str; 5] = ["Б", "КБ", "МБ", "ГБ", "ТБ"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} Б")
    } else {
        format!("{v:.2} {}", U[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{Builder, NO_PARENT};

    #[test]
    fn dates() {
        assert_eq!(format_date(0), "");
        assert_eq!(format_date(86_400 * 59), "01.03.1970");
        assert_eq!(format_date(1_609_459_200), "01.01.2021");
        assert_eq!(format_date(951_782_400), "29.02.2000");
    }

    #[test]
    fn extensions_are_case_insensitive_and_parallel_totals_match() {
        assert_eq!(extension("A.TXT"), "txt");
        assert_eq!(extension(".bashrc"), "");
        assert_eq!(extension("x."), "");
        let mut b = Builder::default();
        let r = b.add(NO_PARENT, "r", Kind::Dir, 0, 0);
        let d = b.add(r, "d", Kind::Dir, 0, 0);
        for i in 0..20_000u64 {
            let name = match i % 3 {
                0 => format!("f{i}.TXT"),
                1 => format!("f{i}.txt"),
                _ => format!("f{i}.Бин"),
            };
            b.add(if i % 2 == 0 { r } else { d }, &name, Kind::File, i, i);
        }
        let t = b.finish();
        let all = type_stats(&t, ROOT);
        let txt = all.iter().find(|s| s.ext == "txt").unwrap();
        assert_eq!(txt.count, 20_000 - 20_000 / 3);
        assert!(all.iter().any(|s| s.ext == "бин"));
        assert_eq!(all.iter().map(|s| s.count).sum::<u64>(), 20_000);
        let sub = type_stats(&t, d);
        assert_eq!(sub.iter().map(|s| s.count).sum::<u64>(), 10_000);
        let top = top_files(&t, 3);
        assert_eq!(
            top.iter().map(|&i| t.node(i).size).collect::<Vec<_>>(),
            vec![19_999, 19_998, 19_997]
        );
    }
}
