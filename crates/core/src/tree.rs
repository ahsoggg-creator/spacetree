//! Flat arena tree: nodes live in one `Vec`, names in one shared `String`,
//! children in a CSR (compressed sparse row) index sorted by size descending.

use rayon::prelude::*;

pub type NodeId = u32;
pub const ROOT: NodeId = 0;
pub const NO_PARENT: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    File = 0,
    Dir = 1,
    /// Symlink / junction / other directory reparse point: listed, never followed, size 0.
    Link = 2,
}

#[derive(Clone, Copy, Debug)]
pub struct Node {
    pub parent: u32,
    pub name_off: u32,
    /// Logical size. For directories: sum over the subtree (filled by `Tree::from_parts`).
    pub size: u64,
    /// Size on disk (allocated). Same aggregation rules as `size`.
    pub alloc: u64,
    /// Number of regular files in the subtree (1 for a file).
    pub files: u32,
    /// Last modification, Unix seconds (0 = unknown). For directories: the latest one in the subtree.
    pub mtime: u32,
    pub name_len: u16,
    pub kind: Kind,
}

impl Node {
    pub fn new(parent: u32, name_off: u32, name_len: u16, kind: Kind, size: u64, alloc: u64) -> Self {
        Node {
            parent,
            name_off,
            size,
            alloc,
            files: 0,
            mtime: 0,
            name_len,
            kind,
        }
    }
}

pub struct Tree {
    nodes: Vec<Node>,
    names: String,
    child_start: Vec<u32>,
    children: Vec<u32>,
    dir_count: u32,
}

impl Tree {
    /// Builds the tree from raw nodes. `nodes[0]` is the root (`parent == NO_PARENT`).
    /// Node order is arbitrary; sizes of directories are aggregated here.
    /// Nodes that are not reachable from the root (parent missing or cycles) are attached to the root.
    pub fn from_parts(mut nodes: Vec<Node>, names: String) -> Tree {
        assert!(!nodes.is_empty(), "tree needs a root");
        nodes[0].parent = NO_PARENT;
        let n = nodes.len();
        let (mut child_start, mut children, mut order) = Self::index(&nodes);
        if order.len() < n {
            let mut seen = vec![false; n];
            for &id in &order {
                seen[id as usize] = true;
            }
            for (i, nd) in nodes.iter_mut().enumerate() {
                if !seen[i] {
                    nd.parent = ROOT;
                }
            }
            (child_start, children, order) = Self::index(&nodes);
            debug_assert_eq!(order.len(), n);
        }

        let mut dir_count = 0u32;
        for nd in nodes.iter_mut() {
            match nd.kind {
                Kind::File => nd.files = 1,
                Kind::Dir => {
                    dir_count += 1;
                    nd.size = 0;
                    nd.alloc = 0;
                    nd.files = 0;
                }
                Kind::Link => {
                    nd.size = 0;
                    nd.alloc = 0;
                    nd.files = 0;
                }
            }
        }
        for &id in order.iter().rev() {
            let nd = nodes[id as usize];
            if nd.parent != NO_PARENT {
                let p = &mut nodes[nd.parent as usize];
                p.size += nd.size;
                p.alloc += nd.alloc;
                p.files += nd.files;
                if nd.kind != Kind::Link {
                    p.mtime = p.mtime.max(nd.mtime);
                }
            }
        }

        Self::sort_children(&nodes, &child_start, &mut children);
        Tree {
            nodes,
            names,
            child_start,
            children,
            dir_count,
        }
    }

    fn index(nodes: &[Node]) -> (Vec<u32>, Vec<u32>, Vec<u32>) {
        let n = nodes.len();
        let mut child_start = vec![0u32; n + 1];
        for (i, nd) in nodes.iter().enumerate() {
            if i != 0 && nd.parent != NO_PARENT {
                child_start[nd.parent as usize + 1] += 1;
            }
        }
        for i in 0..n {
            child_start[i + 1] += child_start[i];
        }
        let mut cursor = child_start.clone();
        let mut children = vec![0u32; child_start[n] as usize];
        for (i, nd) in nodes.iter().enumerate() {
            if i != 0 && nd.parent != NO_PARENT {
                let c = &mut cursor[nd.parent as usize];
                children[*c as usize] = i as u32;
                *c += 1;
            }
        }
        let mut order = Vec::with_capacity(n);
        order.push(ROOT);
        let mut head = 0;
        while head < order.len() {
            let id = order[head] as usize;
            head += 1;
            let (s, e) = (child_start[id] as usize, child_start[id + 1] as usize);
            order.extend_from_slice(&children[s..e]);
        }
        (child_start, children, order)
    }

    fn sort_children(nodes: &[Node], child_start: &[u32], children: &mut [u32]) {
        let mut slices: Vec<&mut [u32]> = Vec::new();
        let mut rest = children;
        for w in child_start.windows(2) {
            let (head, tail) = rest.split_at_mut((w[1] - w[0]) as usize);
            if head.len() > 1 {
                slices.push(head);
            }
            rest = tail;
        }
        slices.par_iter_mut().for_each(|s| {
            s.sort_unstable_by(|&a, &b| nodes[b as usize].size.cmp(&nodes[a as usize].size).then(a.cmp(&b)));
        });
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    pub fn name(&self, id: NodeId) -> &str {
        let n = &self.nodes[id as usize];
        &self.names[n.name_off as usize..n.name_off as usize + n.name_len as usize]
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        let p = self.nodes[id as usize].parent;
        (p != NO_PARENT).then_some(p)
    }

    /// Children sorted by size descending.
    pub fn children(&self, id: NodeId) -> &[NodeId] {
        let (s, e) = (self.child_start[id as usize], self.child_start[id as usize + 1]);
        &self.children[s as usize..e as usize]
    }

    pub fn is_dir(&self, id: NodeId) -> bool {
        self.nodes[id as usize].kind == Kind::Dir
    }

    pub fn dir_count(&self) -> u32 {
        self.dir_count
    }

    pub fn root_stats(&self) -> (u64, u64, u32) {
        let r = &self.nodes[0];
        (r.size, r.alloc, r.files)
    }

    pub fn path(&self, id: NodeId) -> String {
        let mut parts = Vec::new();
        let mut cur = id;
        loop {
            parts.push(self.name(cur));
            match self.parent(cur) {
                Some(p) => cur = p,
                None => break,
            }
        }
        let mut out = String::new();
        for part in parts.iter().rev() {
            if !out.is_empty() && !out.ends_with(std::path::MAIN_SEPARATOR) {
                out.push(std::path::MAIN_SEPARATOR);
            }
            out.push_str(part);
        }
        out
    }

    /// Ancestors from the root down to (and including) `id`.
    pub fn ancestors(&self, id: NodeId) -> Vec<NodeId> {
        let mut v = vec![id];
        let mut cur = id;
        while let Some(p) = self.parent(cur) {
            v.push(p);
            cur = p;
        }
        v.reverse();
        v
    }
}

/// Helper to assemble nodes + shared names buffer.
#[derive(Default)]
pub struct Builder {
    pub nodes: Vec<Node>,
    pub names: String,
}

impl Builder {
    pub fn add(&mut self, parent: u32, name: &str, kind: Kind, size: u64, alloc: u64) -> NodeId {
        let off = self.names.len() as u32;
        self.names.push_str(name);
        let id = self.nodes.len() as u32;
        self.nodes
            .push(Node::new(parent, off, name.len() as u16, kind, size, alloc));
        id
    }

    pub fn finish(self) -> Tree {
        Tree::from_parts(self.nodes, self.names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregates_and_sorts_regardless_of_node_order() {
        let mut b = Builder::default();
        let root = b.add(NO_PARENT, "root", Kind::Dir, 0, 0);
        // child created before its parent directory (arbitrary order, as in MFT)
        let f1 = b.add(2, "a.txt", Kind::File, 10, 4096);
        let d = b.add(root, "d", Kind::Dir, 0, 0);
        let f2 = b.add(root, "big.bin", Kind::File, 100, 4096);
        let f3 = b.add(d, "b.txt", Kind::File, 5, 4096);
        let lnk = b.add(root, "lnk", Kind::Link, 77, 77);
        let t = b.finish();
        assert_eq!(t.root_stats(), (115, 3 * 4096, 3));
        assert_eq!(t.node(d).size, 15);
        assert_eq!(t.node(d).files, 2);
        assert_eq!(t.children(root), &[f2, d, lnk]);
        assert_eq!(t.children(d), &[f1, f3]);
        assert_eq!(t.path(f3), format!("root{0}d{0}b.txt", std::path::MAIN_SEPARATOR));
    }

    #[test]
    fn unreachable_nodes_are_attached_to_root() {
        let mut b = Builder::default();
        b.add(NO_PARENT, "r", Kind::Dir, 0, 0);
        b.add(2, "x", Kind::Dir, 0, 0); // 1 <-> 2 cycle
        b.add(1, "y", Kind::Dir, 0, 0);
        b.add(1, "f", Kind::File, 3, 3);
        let t = b.finish();
        assert_eq!(t.root_stats().0, 3);
        assert!(t.path(3).starts_with('r'));
    }
}
