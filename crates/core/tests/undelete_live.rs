//! Needs administrator rights and a real NTFS volume: `SPACETREE_UNDELETE_DRIVE=D cargo test -p spacetree-core
//! --test undelete_live -- --ignored --nocapture`. Restores the smallest intact file into a temp folder.
#![cfg(windows)]

use spacetree_core::Progress;
use spacetree_core::undelete::{self, Chance};

#[test]
#[ignore = "needs administrator rights"]
fn scans_a_live_volume_and_restores_one_file() {
    let drive = std::env::var("SPACETREE_UNDELETE_DRIVE")
        .ok()
        .and_then(|s| s.chars().next())
        .unwrap_or('C');
    let t = std::time::Instant::now();
    let scan = undelete::scan(drive, &Progress::default()).expect("scan");
    let good = scan.files.iter().filter(|f| f.chance == Chance::Good).count();
    println!(
        "{} records, {} candidates, {good} intact, {} skipped, {:?}",
        scan.records,
        scan.files.len(),
        scan.skipped,
        t.elapsed()
    );
    for f in scan.files.iter().filter(|f| f.chance == Chance::Good).take(5) {
        println!("  {} ({} B)", f.path(), f.size);
    }
    let t = std::time::Instant::now();
    let carved = spacetree_core::carve::carve(
        drive,
        &scan.files.iter().filter_map(|f| f.first_lcn()).collect(),
        &Progress::default(),
    )
    .expect("carve");
    println!("carve: {} files in {:?}", carved.len(), t.elapsed());
    for f in carved.iter().take(5) {
        println!("  {} ({} B)", f.name, f.size);
    }
    let pick = scan
        .files
        .iter()
        .filter(|f| f.chance == Chance::Good && f.size > 0)
        .min_by_key(|f| f.size);
    if let Some(f) = pick {
        let dest = tempfile::tempdir().unwrap();
        if dest.path().to_string_lossy().starts_with(drive) {
            println!("temp dir is on the scanned drive, skipping restore");
            return;
        }
        let vol = undelete::Volume::open(drive).expect("open");
        let r = vol
            .restore(f, dest.path(), true, &Progress::default())
            .expect("restore");
        println!(
            "restored {} -> {} ({} B, all_zero={})",
            f.path(),
            r.path.display(),
            r.bytes,
            r.all_zero
        );
        assert_eq!(std::fs::metadata(&r.path).unwrap().len(), f.size);
    }
}
