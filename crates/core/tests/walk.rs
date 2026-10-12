use std::fs;
use std::path::{Path, PathBuf};

use spacetree_core::tree::Kind;
use spacetree_core::{Backend, NodeId, Progress, Tree, export, scan, stats};

fn write(path: &Path, len: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![b'x'; len]).unwrap();
}

/// Returns (file count, total bytes, long path file).
fn build_fixture(root: &Path) -> (u64, u64) {
    let mut files = 0u64;
    let mut bytes = 0u64;
    let mut add = |rel: &str, len: usize| {
        write(&root.join(rel), len);
        files += 1;
        bytes += len as u64;
    };
    add("empty.txt", 0);
    add("a/file1.bin", 1000);
    add("a/b/c/юникод-файл.txt", 2500);
    add("папка с пробелом/файл №1 😀.dat", 4097);
    add("top.bin", 1);
    fs::create_dir_all(root.join("empty_dir/nested_empty")).unwrap();

    let mut long = root.to_path_buf();
    for i in 0..14 {
        long.push(format!("long_directory_name_{i:02}_xxxxxxxxxxxx"));
    }
    assert!(long.as_os_str().len() > 260, "fixture path must exceed MAX_PATH");
    let long_file = long.join("deep.bin");
    write(&long_file, 777);
    files += 1;
    bytes += 777;
    (files, bytes)
}

fn check_invariants(t: &Tree) {
    for id in 0..t.len() as NodeId {
        let nd = t.node(id);
        if nd.kind != Kind::Dir {
            continue;
        }
        let (mut s, mut a, mut f) = (0u64, 0u64, 0u32);
        for &c in t.children(id) {
            let cn = t.node(c);
            s += cn.size;
            a += cn.alloc;
            f += cn.files;
            assert_eq!(cn.parent, id);
        }
        assert_eq!(
            (nd.size, nd.alloc, nd.files),
            (s, a, f),
            "invariant broken at {}",
            t.path(id)
        );
        let sizes: Vec<u64> = t.children(id).iter().map(|&c| t.node(c).size).collect();
        assert!(
            sizes.windows(2).all(|w| w[0] >= w[1]),
            "children must be sorted by size desc"
        );
    }
}

fn scan_walk(p: &Path) -> spacetree_core::ScanOutput {
    scan(p, Backend::Walk, &Progress::default()).unwrap()
}

#[test]
fn counts_sizes_and_invariants_match_fixture() {
    let dir = tempfile::tempdir().unwrap();
    let (files, bytes) = build_fixture(dir.path());
    let out = scan_walk(dir.path());
    let (size, alloc, nfiles) = out.tree.root_stats();
    assert_eq!(nfiles as u64, files);
    assert_eq!(size, bytes);
    assert!(alloc >= size || cfg!(unix)); // unix: st_blocks may be smaller for sparse/inline files
    assert_eq!(out.errors, 0, "{:?}", out.error_samples);
    check_invariants(&out.tree);

    let top = stats::top_files(&out.tree, 1);
    assert_eq!(out.tree.name(top[0]), "файл №1 😀.dat");
    let ts = stats::type_stats(&out.tree, 0);
    assert_eq!(ts.iter().map(|t| t.count).sum::<u64>(), files);

    let mut csv = Vec::new();
    export::write_csv(&out.tree, &mut csv).unwrap();
    let csv = String::from_utf8(csv).unwrap();
    assert!(csv.starts_with("path,size,size_on_disk,files\n"));
    assert!(csv.contains(",4097,"));
    assert_eq!(csv.lines().count(), out.tree.len() + 1);
}

fn try_symlink_dir(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    let r = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let r = std::os::windows::fs::symlink_dir(target, link).or_else(|_| {
        // no symlink privilege: a junction needs none and exercises the same reparse-point path
        let ok = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            Ok(())
        } else {
            Err(std::io::Error::other("mklink /J failed"))
        }
    });
    r.is_ok()
}

#[test]
fn symlink_to_parent_is_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    let (files, bytes) = build_fixture(dir.path());
    let link: PathBuf = dir.path().join("a/b/loop_to_root").components().collect();
    if !try_symlink_dir(dir.path(), &link) {
        eprintln!("SKIPPED symlink_to_parent_is_not_followed: OS did not allow creating a symlink");
        return;
    }
    let out = scan_walk(dir.path());
    let (size, _, nfiles) = out.tree.root_stats();
    assert_eq!((nfiles as u64, size), (files, bytes));
    let n = out.tree.nodes().iter().filter(|n| n.kind == Kind::Link).count();
    assert_eq!(n, 1);
    check_invariants(&out.tree);
    let _ = fs::remove_dir(&link); // detach first so cleanup can never follow the link
}

#[cfg(windows)]
fn deny_access(dir: &Path) -> bool {
    std::process::Command::new("icacls")
        .arg(dir)
        .args(["/deny", "*S-1-1-0:(RD)"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(windows)]
fn restore_access(dir: &Path) {
    let _ = std::process::Command::new("icacls")
        .arg(dir)
        .args(["/reset", "/T"])
        .output();
}

#[cfg(unix)]
fn deny_access(dir: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o000)).is_ok()
}

#[cfg(unix)]
fn restore_access(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o755));
}

#[test]
fn unreadable_directory_is_counted_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let (files, bytes) = build_fixture(dir.path());
    let locked: PathBuf = dir.path().join("locked");
    write(&locked.join("secret.bin"), 5000);
    let denied = deny_access(&locked) && fs::read_dir(&locked).is_err();
    if !denied {
        restore_access(&locked);
        eprintln!(
            "SKIPPED unreadable_directory_is_counted_not_fatal: cannot make a directory unreadable (root/admin?)"
        );
        return;
    }
    let out = scan_walk(dir.path());
    restore_access(&locked);
    assert!(out.errors >= 1, "access error must be counted");
    assert!(!out.error_samples.is_empty());
    let (size, _, nfiles) = out.tree.root_stats();
    assert_eq!((nfiles as u64, size), (files, bytes));
    check_invariants(&out.tree);
}

#[test]
fn nonexistent_root_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    assert!(scan(&dir.path().join("nope"), Backend::Walk, &Progress::default()).is_err());
}

#[test]
fn cancel_returns_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    build_fixture(dir.path());
    let p = Progress::default();
    p.cancel();
    assert!(matches!(
        scan(dir.path(), Backend::Walk, &p),
        Err(spacetree_core::ScanError::Cancelled)
    ));
}
