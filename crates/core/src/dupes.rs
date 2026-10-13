//! Duplicate files by content: same size, then same hash of the first bytes, then same hash of the whole file.
//!
//! Every stage only keeps the files that still have a possible twin, so most files are never read at all
//! and the rest are read only as far as needed to tell them apart.

use std::fs::File;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering::Relaxed};

use rayon::prelude::*;

use crate::tree::{NodeId, Tree};

/// Bytes hashed in the cheap middle stage.
const PARTIAL_LEN: usize = 64 << 10;
const BUF_LEN: usize = 1 << 20;
/// Parallel readers: enough to keep an SSD busy, few enough not to make a spinning disk seek itself to death.
const READERS: usize = 4;

pub const STAGE_SIZES: u8 = 0;
pub const STAGE_PARTIAL: u8 = 1;
pub const STAGE_FULL: u8 = 2;

/// Files with identical content.
#[derive(Clone, Debug)]
pub struct DupSet {
    pub size: u64,
    /// Newest first (by modification time): `items[0]` is the copy to keep, the rest are redundant.
    pub items: Vec<NodeId>,
}

impl DupSet {
    pub fn wasted(&self) -> u64 {
        self.size * (self.items.len() as u64 - 1)
    }
}

/// Shared between the searching thread and the observer.
#[derive(Default)]
pub struct DupProgress {
    cancel: AtomicBool,
    pub stage: AtomicU8,
    /// Files to look at / looked at in the current stage.
    pub total: AtomicU64,
    pub done: AtomicU64,
    /// Bytes read so far.
    pub bytes: AtomicU64,
}

impl DupProgress {
    pub fn cancel(&self) {
        self.cancel.store(true, Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Relaxed)
    }

    fn start(&self, stage: u8, total: usize) {
        self.stage.store(stage, Relaxed);
        self.total.store(total as u64, Relaxed);
        self.done.store(0, Relaxed);
    }
}

/// Reads `limit` bytes (or the whole file) and returns their hash; `None` if the file cannot be read,
/// is not `size` bytes long any more, or the search was cancelled.
fn hash_file(path: &str, size: u64, limit: usize, progress: &DupProgress) -> Option<blake3::Hash> {
    let mut f = File::open(path).ok()?;
    if f.metadata().ok()?.len() != size {
        return None;
    }
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; limit.min(BUF_LEN)];
    let mut left = limit as u64;
    while left > 0 {
        if progress.is_cancelled() {
            return None;
        }
        let want = left.min(buf.len() as u64) as usize;
        let n = f.read(&mut buf[..want]).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        progress.bytes.fetch_add(n as u64, Relaxed);
        left -= n as u64;
    }
    Some(hasher.finalize())
}

/// Keeps the groups of `keyed` (sorted by key) that have at least two members.
fn twins<K: PartialEq + Ord + Copy>(mut keyed: Vec<(K, NodeId)>) -> Vec<(K, Vec<NodeId>)> {
    keyed.sort_unstable();
    keyed
        .chunk_by(|a, b| a.0 == b.0)
        .filter(|g| g.len() > 1)
        .map(|g| (g[0].0, g.iter().map(|x| x.1).collect()))
        .collect()
}

/// Narrows `groups` down by hashing `limit` bytes of each file; returns groups that are still ambiguous.
fn refine(
    tree: &Tree,
    groups: Vec<(u64, Vec<NodeId>)>,
    limit: usize,
    stage: u8,
    progress: &DupProgress,
    pool: &rayon::ThreadPool,
) -> Vec<(u64, Vec<NodeId>)> {
    progress.start(stage, groups.iter().map(|g| g.1.len()).sum());
    let hashed: Vec<(u64, [u8; 32], NodeId)> = pool.install(|| {
        groups
            .par_iter()
            .flat_map_iter(|(size, ids)| ids.iter().map(move |&id| (*size, id)))
            .filter_map(|(size, id)| {
                let h = hash_file(&tree.path(id), size, limit, progress);
                progress.done.fetch_add(1, Relaxed);
                h.map(|h| (size, *h.as_bytes(), id))
            })
            .collect()
    });
    if progress.is_cancelled() {
        return Vec::new();
    }
    twins(hashed.into_iter().map(|(s, h, id)| ((s, h), id)).collect())
        .into_iter()
        .map(|((size, _), ids)| (size, ids))
        .collect()
}

/// Finds sets of files with identical content among `candidates` (regular files of `tree`).
/// Sets are ordered by wasted space, largest first. `None` when cancelled.
pub fn find(tree: &Tree, candidates: &[NodeId], progress: &DupProgress) -> Option<Vec<DupSet>> {
    progress.start(STAGE_SIZES, candidates.len());
    let by_size = twins(candidates.iter().map(|&id| (tree.node(id).size, id)).collect());
    progress.done.store(candidates.len() as u64, Relaxed);

    let pool = rayon::ThreadPoolBuilder::new().num_threads(READERS).build().ok()?;
    let partial = refine(tree, by_size, PARTIAL_LEN, STAGE_PARTIAL, progress, &pool);
    let full = refine(tree, partial, usize::MAX, STAGE_FULL, progress, &pool);
    if progress.is_cancelled() {
        return None;
    }

    let mut sets: Vec<DupSet> = full
        .into_iter()
        .map(|(size, mut items)| {
            items.sort_by_key(|&i| (std::cmp::Reverse(tree.node(i).mtime), i));
            DupSet { size, items }
        })
        .collect();
    sets.sort_by_key(|s| std::cmp::Reverse(s.wasted()));
    Some(sets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{Builder, Kind, NO_PARENT};

    fn write(dir: &std::path::Path, name: &str, data: &[u8]) {
        std::fs::write(dir.join(name), data).unwrap();
    }

    #[test]
    fn finds_only_files_with_identical_content() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let len = 200 << 10;
        let base = vec![7u8; len];
        let mut tail_differs = base.clone();
        *tail_differs.last_mut().unwrap() = 8; // same first 64 KiB: only the full hash tells them apart
        let mut head_differs = base.clone();
        head_differs[0] = 9;
        write(root, "a.bin", &base);
        write(root, "b.bin", &base);
        write(root, "c.bin", &tail_differs);
        write(root, "d.bin", &head_differs);
        write(root, "e.bin", &base[..len - 1]);
        write(root, "f.bin", &base);

        let mut b = Builder::default();
        let r = b.add(NO_PARENT, root.to_str().unwrap(), Kind::Dir, 0, 0);
        let ids: Vec<NodeId> = ["a.bin", "b.bin", "c.bin", "d.bin", "e.bin", "f.bin"]
            .iter()
            .map(|n| b.add(r, n, Kind::File, std::fs::metadata(root.join(n)).unwrap().len(), 0))
            .collect();
        // a.bin is the oldest, f.bin the newest
        for (i, &id) in ids.iter().enumerate() {
            b.nodes[id as usize].mtime = 100 + i as u32;
        }
        let tree = b.finish();

        let progress = DupProgress::default();
        let sets = find(&tree, &ids, &progress).unwrap();
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].items, vec![ids[5], ids[1], ids[0]], "newest first");
        assert_eq!(sets[0].wasted(), 2 * len as u64);
    }

    #[test]
    fn unreadable_and_cancelled() {
        let mut b = Builder::default();
        let r = b.add(NO_PARENT, "/nonexistent-spacetree-test", Kind::Dir, 0, 0);
        let x = b.add(r, "x", Kind::File, 5, 0);
        let y = b.add(r, "y", Kind::File, 5, 0);
        let tree = b.finish();
        let p = DupProgress::default();
        assert!(find(&tree, &[x, y], &p).unwrap().is_empty());
        p.cancel();
        assert!(find(&tree, &[x, y], &p).is_none());
    }
}
