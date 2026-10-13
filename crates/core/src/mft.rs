//! NTFS backend: reads `$MFT` straight from the raw volume (read-only) and rebuilds the tree
//! from parent references. Requires administrator rights.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::{FileExt, OpenOptionsExt};
use std::sync::Mutex;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc::sync_channel;

use rayon::prelude::*;

use crate::error::ScanError;
use crate::ntfs::{self, ATTR_ATTRIBUTE_LIST, ATTR_DATA, ChunkOut, Run};
use crate::progress::{PHASE_BUILDING, Progress};
use crate::tree::Tree;
use crate::winapi;

const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_INVALID_FUNCTION: i32 = 1;
const SHARE_ALL: u32 = 0x1 | 0x2 | 0x4; // FILE_SHARE_READ | WRITE | DELETE
const CHUNK: u64 = 8 * 1024 * 1024;
const RECORDS_PER_TASK: usize = 2048;
const MAX_ERROR_SAMPLES: usize = 200;

pub struct MftOutput {
    pub tree: Tree,
    pub errors: u64,
    pub error_samples: Vec<String>,
    pub extra_links: u64,
}

pub struct VolumeInfo {
    bytes_per_cluster: u64,
    record_size: usize,
    mft_start_lcn: u64,
}

/// Opens `\\.\X:` read-only and checks that it is NTFS.
pub fn probe(drive: char) -> Result<(File, VolumeInfo), ScanError> {
    let path = format!(r"\\.\{drive}:");
    let file = OpenOptions::new()
        .read(true)
        .share_mode(SHARE_ALL)
        .open(&path)
        .map_err(|e| {
            if e.raw_os_error() == Some(ERROR_ACCESS_DENIED) {
                ScanError::Unsupported(format!("нет прав администратора для чтения тома {drive}: ({e})"))
            } else {
                ScanError::Io(format!("не удалось открыть том {drive}: ({e})"))
            }
        })?;
    let v = winapi::ntfs_volume_data(&file).map_err(|e| {
        if e.raw_os_error() == Some(ERROR_INVALID_FUNCTION) {
            ScanError::Unsupported(format!("том {drive}: не NTFS"))
        } else {
            ScanError::Io(format!("FSCTL_GET_NTFS_VOLUME_DATA для {drive}: ({e})"))
        }
    })?;
    let (bpc, rs, lcn) = (
        v.BytesPerCluster as u64,
        v.BytesPerFileRecordSegment as usize,
        v.MftStartLcn,
    );
    if bpc == 0 || rs < 512 || !rs.is_power_of_two() || lcn <= 0 {
        return Err(ScanError::Ntfs("некорректные параметры тома".into()));
    }
    Ok((
        file,
        VolumeInfo {
            bytes_per_cluster: bpc,
            record_size: rs,
            mft_start_lcn: lcn as u64,
        },
    ))
}

fn read_exact_at(file: &File, mut buf: &mut [u8], mut off: u64) -> io::Result<()> {
    while !buf.is_empty() {
        let n = file.seek_read(buf, off)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buf = &mut buf[n..];
        off += n as u64;
    }
    Ok(())
}

/// Reads `buf.len()` bytes of the $MFT data stream starting at virtual offset `voff`.
fn read_virtual(file: &File, runs: &[Run], bpc: u64, mut voff: u64, buf: &mut [u8]) -> io::Result<()> {
    let mut done = 0;
    while done < buf.len() {
        let vcn = voff / bpc;
        let run = runs
            .iter()
            .find(|r| vcn >= r.vcn && vcn < r.vcn + r.len)
            .ok_or_else(|| io::Error::other("смещение вне data runs $MFT"))?;
        let into = voff - run.vcn * bpc;
        let n = ((run.len * bpc - into) as usize).min(buf.len() - done);
        let dst = &mut buf[done..done + n];
        match run.lcn {
            Some(l) => read_exact_at(file, dst, l * bpc + into)?,
            None => dst.fill(0),
        }
        done += n;
        voff += n as u64;
    }
    Ok(())
}

fn data_extents(attr: &[u8]) -> Result<(Vec<Run>, u64), ScanError> {
    if attr[8] == 0 || attr.len() < 0x40 {
        return Err(ScanError::Ntfs("$DATA файла $MFT резидентный".into()));
    }
    let start = u64::from_le_bytes(attr[0x10..0x18].try_into().unwrap());
    let real = u64::from_le_bytes(attr[0x30..0x38].try_into().unwrap());
    let roff = u16::from_le_bytes([attr[0x20], attr[0x21]]) as usize;
    let mut runs = ntfs::parse_runs(attr.get(roff..).unwrap_or(&[]))?;
    for r in &mut runs {
        r.vcn += start;
    }
    Ok((runs, real))
}

/// Reads record 0 ($MFT itself) and returns the extents of its $DATA stream and its real size.
fn mft_extents(file: &File, info: &VolumeInfo) -> Result<(Vec<Run>, u64), ScanError> {
    let bpc = info.bytes_per_cluster;
    let rs = info.record_size;
    let mut block = vec![0u8; rs.max(4096)];
    read_exact_at(file, &mut block, info.mft_start_lcn * bpc)?;
    if !ntfs::apply_fixups(&mut block[..rs]) {
        return Err(ScanError::Ntfs("запись 0 ($MFT) повреждена".into()));
    }
    let rec0 = &block[..rs];

    let first = ntfs::find_attr(rec0, ATTR_DATA).ok_or_else(|| ScanError::Ntfs("в $MFT нет атрибута $DATA".into()))?;
    let (mut runs, real_size) = data_extents(first)?;

    // A heavily fragmented $MFT spills its $DATA extents into extension records listed in $ATTRIBUTE_LIST.
    if let Some(list) = ntfs::find_attr(rec0, ATTR_ATTRIBUTE_LIST) {
        if list[8] != 0 {
            return Err(ScanError::Unsupported(
                "нерезидентный $ATTRIBUTE_LIST у $MFT не поддерживается".into(),
            ));
        }
        let coff = u16::from_le_bytes([list[0x14], list[0x15]]) as usize;
        let clen = u32::from_le_bytes(list[0x10..0x14].try_into().unwrap()) as usize;
        let content = list
            .get(coff..coff + clen)
            .ok_or_else(|| ScanError::Ntfs("битый $ATTRIBUTE_LIST".into()))?;
        let mut seen = vec![0u64];
        for (ty, _vcn, rec_no) in ntfs::parse_attribute_list(content) {
            if ty != ATTR_DATA || seen.contains(&rec_no) {
                continue;
            }
            seen.push(rec_no);
            runs.sort_by_key(|r| r.vcn);
            let mut ext = vec![0u8; rs];
            read_virtual(file, &runs, bpc, rec_no * rs as u64, &mut ext)?;
            if !ntfs::apply_fixups(&mut ext) {
                return Err(ScanError::Ntfs(format!("расширяющая запись {rec_no} $MFT повреждена")));
            }
            if let Some(a) = ntfs::find_attr(&ext, ATTR_DATA) {
                runs.extend(data_extents(a)?.0);
            }
        }
        runs.sort_by_key(|r| r.vcn);
    }
    Ok((runs, real_size))
}

pub fn scan(drive: char, progress: &Progress) -> Result<MftOutput, ScanError> {
    let (file, info) = probe(drive)?;
    let (runs, real_size) = mft_extents(&file, &info)?;
    let (bpc, rs) = (info.bytes_per_cluster, info.record_size);
    let alloc_total: u64 = runs.iter().map(|r| r.len).sum::<u64>() * bpc;
    let total_records = real_size / rs as u64;
    progress.total_units.store(total_records, Relaxed);

    let errors: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let note = |msg: String| {
        progress.errors.fetch_add(1, Relaxed);
        let mut v = errors.lock().unwrap();
        if v.len() < MAX_ERROR_SAMPLES {
            v.push(msg);
        }
    };

    let (tx, rx) = sync_channel::<(u64, Vec<u8>)>(3);
    let mut outs: Vec<ChunkOut> = Vec::new();
    std::thread::scope(|s| {
        s.spawn(|| {
            let mut voff = 0u64;
            while voff < real_size && !progress.is_cancelled() {
                let n = CHUNK.min(alloc_total - voff);
                let mut buf = vec![0u8; n as usize];
                if let Err(e) = read_virtual(&file, &runs, bpc, voff, &mut buf) {
                    note(format!("чтение $MFT по смещению {voff}: {e}"));
                    buf.fill(0);
                }
                if tx.send((voff / rs as u64, buf)).is_err() {
                    break;
                }
                voff += n;
            }
            drop(tx); // closes the channel so the consumer loop ends
        });
        for (first, mut buf) in rx {
            if progress.is_cancelled() {
                break; // dropping `rx` stops the reader
            }
            let valid = ((total_records - first) as usize).min(buf.len() / rs);
            let task = RECORDS_PER_TASK * rs;
            let parsed: Vec<ChunkOut> = buf[..valid * rs]
                .par_chunks_mut(task)
                .enumerate()
                .map(|(i, part)| {
                    let mut out = ChunkOut::default();
                    let base = first + (i * RECORDS_PER_TASK) as u64;
                    for (j, rec) in part.chunks_exact_mut(rs).enumerate() {
                        ntfs::parse_record(rec, (base + j as u64) as u32, &mut out);
                    }
                    out
                })
                .collect();
            let named = parsed.iter().map(|c| c.bases.len() as u64).sum::<u64>();
            progress.files.fetch_add(named, Relaxed);
            progress.done_units.fetch_add(valid as u64, Relaxed);
            outs.extend(parsed);
        }
    });
    if progress.is_cancelled() {
        return Err(ScanError::Cancelled);
    }

    progress.phase.store(PHASE_BUILDING, Relaxed);
    let built = ntfs::build_tree(outs, &format!("{drive}:\\"))?;
    Ok(MftOutput {
        tree: built.tree,
        errors: progress.errors.load(Relaxed),
        error_samples: errors.into_inner().unwrap(),
        extra_links: built.extra_links,
    })
}
