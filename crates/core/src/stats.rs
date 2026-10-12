use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use crate::tree::{Kind, NodeId, Tree};

/// Lower-cased extension without the dot; empty string when there is none.
pub fn extension(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => name[i + 1..].to_lowercase(),
        _ => String::new(),
    }
}

/// Largest `n` files, biggest first.
pub fn top_files(tree: &Tree, n: usize) -> Vec<NodeId> {
    let mut heap: BinaryHeap<Reverse<(u64, NodeId)>> = BinaryHeap::with_capacity(n + 1);
    for (i, nd) in tree.nodes().iter().enumerate() {
        if nd.kind != Kind::File {
            continue;
        }
        let item = Reverse((nd.size, i as NodeId));
        if heap.len() < n {
            heap.push(item);
        } else if let Some(min) = heap.peek()
            && item < *min
        {
            heap.pop();
            heap.push(item);
        }
    }
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

/// Per-extension totals over the files of the subtree below `root`, sorted by size descending.
pub fn type_stats(tree: &Tree, root: NodeId) -> Vec<TypeStat> {
    let mut map: HashMap<String, TypeStat> = HashMap::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        let nd = tree.node(id);
        match nd.kind {
            Kind::Dir => stack.extend_from_slice(tree.children(id)),
            Kind::File => {
                let ext = extension(tree.name(id));
                let e = map.entry(ext.clone()).or_insert(TypeStat {
                    ext,
                    size: 0,
                    alloc: 0,
                    count: 0,
                });
                e.size += nd.size;
                e.alloc += nd.alloc;
                e.count += 1;
            }
            Kind::Link => {}
        }
    }
    let mut v: Vec<TypeStat> = map.into_values().collect();
    v.sort_unstable_by(|a, b| b.size.cmp(&a.size).then_with(|| a.ext.cmp(&b.ext)));
    v
}
