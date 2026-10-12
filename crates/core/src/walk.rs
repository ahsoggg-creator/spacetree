//! Parallel directory walker (rayon). Never follows symlinks, junctions or other directory reparse points.
//!
//! Size on disk: Unix — `st_blocks * 512`; Windows — logical size rounded up to the volume cluster size
//! (no per-file syscalls; compressed/sparse files and MFT-resident files are therefore approximated).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

use crate::error::ScanError;
use crate::progress::{PHASE_BUILDING, Progress};
use crate::tree::{Kind, NO_PARENT, Node, Tree};

const MAX_ERROR_SAMPLES: usize = 200;

#[derive(Default)]
struct Sink {
    nodes: Vec<(u32, Node)>,
    names: String,
}

struct Ctx<'a> {
    progress: &'a Progress,
    next_id: AtomicU32,
    sinks: Vec<Mutex<Sink>>,
    errors: Mutex<Vec<String>>,
    cluster: u64,
    #[cfg(unix)]
    seen_links: Mutex<std::collections::HashSet<(u64, u64)>>,
}

impl Ctx<'_> {
    fn error(&self, path: &Path, e: &std::io::Error) {
        self.progress.errors.fetch_add(1, Relaxed);
        let mut v = self.errors.lock().unwrap();
        if v.len() < MAX_ERROR_SAMPLES {
            v.push(format!("{}: {}", path.display(), e));
        }
    }
}

struct Entry {
    name: String,
    kind: Kind,
    size: u64,
    alloc: u64,
}

pub struct WalkOutput {
    pub tree: Tree,
    pub errors: u64,
    pub error_samples: Vec<String>,
}

pub fn scan(root: &Path, progress: &Progress) -> Result<WalkOutput, ScanError> {
    let root = std::path::absolute(root)?;
    let meta = fs::metadata(&root).map_err(|e| ScanError::Io(format!("{}: {e}", root.display())))?;
    if !meta.is_dir() {
        return Err(ScanError::Io(format!("{}: не папка", root.display())));
    }

    let threads = (std::thread::available_parallelism().map_or(4, |n| n.get()) * 2).clamp(4, 32);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(|e| ScanError::Io(e.to_string()))?;

    let ctx = Ctx {
        progress,
        next_id: AtomicU32::new(1),
        sinks: (0..=threads).map(|_| Mutex::new(Sink::default())).collect(),
        errors: Mutex::new(Vec::new()),
        cluster: cluster_size(&root),
        #[cfg(unix)]
        seen_links: Mutex::new(Default::default()),
    };

    pool.install(|| {
        rayon::scope(|s| scan_dir(&ctx, s, root.clone(), 0));
    });
    if progress.is_cancelled() {
        return Err(ScanError::Cancelled);
    }

    progress.phase.store(PHASE_BUILDING, Relaxed);
    let total = ctx.next_id.load(Relaxed) as usize;
    let placeholder = Node::new(NO_PARENT, 0, 0, Kind::File, 0, 0);
    let root_name = root.to_string_lossy().into_owned();
    let mut names = root_name.clone();
    let mut nodes = vec![placeholder; total];
    nodes[0] = Node::new(NO_PARENT, 0, root_name.len() as u16, Kind::Dir, 0, 0);
    for sink in ctx.sinks {
        let sink = sink.into_inner().unwrap();
        let base = names.len() as u32;
        names.push_str(&sink.names);
        for (id, mut n) in sink.nodes {
            n.name_off += base;
            nodes[id as usize] = n;
        }
    }
    let errors = progress.errors.load(Relaxed);
    let error_samples = ctx.errors.into_inner().unwrap();
    Ok(WalkOutput {
        tree: Tree::from_parts(nodes, names),
        errors,
        error_samples,
    })
}

fn cluster_size(root: &Path) -> u64 {
    #[cfg(windows)]
    {
        crate::winapi::cluster_size(root).unwrap_or(4096)
    }
    #[cfg(not(windows))]
    {
        let _ = root;
        1
    }
}

fn scan_dir<'s>(ctx: &'s Ctx<'s>, scope: &rayon::Scope<'s>, path: PathBuf, id: u32) {
    if ctx.progress.is_cancelled() {
        return;
    }
    let rd = match fs::read_dir(&path) {
        Ok(rd) => rd,
        Err(e) => {
            ctx.error(&path, &e);
            return;
        }
    };

    let mut entries: Vec<Entry> = Vec::new();
    for ent in rd {
        let ent = match ent {
            Ok(e) => e,
            Err(e) => {
                ctx.error(&path, &e);
                continue;
            }
        };
        let ft = match ent.file_type() {
            Ok(ft) => ft,
            Err(e) => {
                ctx.error(&ent.path(), &e);
                continue;
            }
        };
        let name = ent.file_name().to_string_lossy().into_owned();
        if ft.is_symlink() {
            entries.push(Entry {
                name,
                kind: Kind::Link,
                size: 0,
                alloc: 0,
            });
            continue;
        }
        let meta = match ent.metadata() {
            Ok(m) => m,
            Err(e) => {
                ctx.error(&ent.path(), &e);
                continue;
            }
        };
        if ft.is_dir() {
            let kind = if is_reparse_point(&meta) { Kind::Link } else { Kind::Dir };
            entries.push(Entry {
                name,
                kind,
                size: 0,
                alloc: 0,
            });
        } else {
            let Some((size, alloc)) = file_sizes(ctx, &meta) else {
                continue;
            };
            entries.push(Entry {
                name,
                kind: Kind::File,
                size,
                alloc,
            });
        }
    }
    if entries.is_empty() {
        return;
    }

    let first = ctx.next_id.fetch_add(entries.len() as u32, Relaxed);
    let (mut files, mut bytes) = (0u64, 0u64);
    {
        let idx = rayon::current_thread_index().map_or(0, |i| i + 1);
        let mut sink = ctx.sinks[idx.min(ctx.sinks.len() - 1)].lock().unwrap();
        for (i, e) in entries.iter().enumerate() {
            let off = sink.names.len() as u32;
            sink.names.push_str(&e.name);
            let node = Node::new(id, off, e.name.len() as u16, e.kind, e.size, e.alloc);
            sink.nodes.push((first + i as u32, node));
            if e.kind == Kind::File {
                files += 1;
                bytes += e.size;
            }
        }
    }
    ctx.progress.files.fetch_add(files, Relaxed);
    ctx.progress.bytes.fetch_add(bytes, Relaxed);

    for (i, e) in entries.into_iter().enumerate() {
        if e.kind == Kind::Dir {
            let child = path.join(&e.name);
            let child_id = first + i as u32;
            scope.spawn(move |s| scan_dir(ctx, s, child, child_id));
        }
    }
}

#[cfg(windows)]
fn is_reparse_point(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_: &fs::Metadata) -> bool {
    false
}

/// Returns `None` when the file is a hard link already counted elsewhere (Unix only).
fn file_sizes(ctx: &Ctx, meta: &fs::Metadata) -> Option<(u64, u64)> {
    let size = meta.len();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() > 1 && !ctx.seen_links.lock().unwrap().insert((meta.dev(), meta.ino())) {
            return None;
        }
        Some((size, meta.blocks() * 512))
    }
    #[cfg(not(unix))]
    {
        let alloc = if size == 0 {
            0
        } else {
            size.div_ceil(ctx.cluster) * ctx.cluster
        };
        Some((size, alloc))
    }
}
