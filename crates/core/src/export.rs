use std::io::{self, Write};

use crate::tree::{ROOT, Tree};

fn field(w: &mut impl Write, s: &str) -> io::Result<()> {
    if s.contains([',', '"', '\n', '\r']) {
        write!(w, "\"{}\"", s.replace('"', "\"\""))
    } else {
        w.write_all(s.as_bytes())
    }
}

/// Writes `path,size,size_on_disk,files` for every node (depth-first, larger siblings first).
/// Directory paths end with a path separator.
pub fn write_csv(tree: &Tree, w: &mut impl Write) -> io::Result<()> {
    writeln!(w, "path,size,size_on_disk,files")?;
    let sep = std::path::MAIN_SEPARATOR;
    let mut path = String::new();
    let mut stack: Vec<(u32, usize)> = vec![(ROOT, 0)];
    while let Some((id, plen)) = stack.pop() {
        path.truncate(plen);
        path.push_str(tree.name(id));
        let is_dir = tree.is_dir(id);
        if is_dir && !path.ends_with(sep) {
            path.push(sep);
        }
        let nd = tree.node(id);
        field(w, &path)?;
        writeln!(w, ",{},{},{}", nd.size, nd.alloc, nd.files)?;
        if is_dir {
            let l = path.len();
            for &c in tree.children(id).iter().rev() {
                stack.push((c, l));
            }
        }
    }
    Ok(())
}
