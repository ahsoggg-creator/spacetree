//! Thin WinAPI wrappers. All `unsafe` of the crate lives here.

use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceW, GetVolumePathNameW};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{FSCTL_GET_NTFS_VOLUME_DATA, NTFS_VOLUME_DATA_BUFFER};

#[repr(C)]
struct VolumeDataOut {
    base: NTFS_VOLUME_DATA_BUFFER,
    // NTFS_EXTENDED_VOLUME_DATA may follow the base structure; reserve room for it.
    extended: [u8; 64],
}

/// FSCTL_GET_NTFS_VOLUME_DATA; fails with `ERROR_INVALID_FUNCTION` on non-NTFS volumes.
pub fn ntfs_volume_data(volume: &File) -> io::Result<NTFS_VOLUME_DATA_BUFFER> {
    // SAFETY: `VolumeDataOut` is plain-old-data, all-zero is a valid value.
    let mut out: VolumeDataOut = unsafe { std::mem::zeroed() };
    let mut returned = 0u32;
    // SAFETY: the handle is valid for the lifetime of `volume`; the output pointer/size describe
    // `out`, which outlives the call; no input buffer is used (null, 0); no OVERLAPPED is passed
    // and the handle was opened synchronously.
    let ok = unsafe {
        DeviceIoControl(
            volume.as_raw_handle(),
            FSCTL_GET_NTFS_VOLUME_DATA,
            std::ptr::null(),
            0,
            &mut out as *mut VolumeDataOut as *mut c_void,
            std::mem::size_of::<VolumeDataOut>() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(out.base)
}

fn wide(p: &Path) -> Vec<u16> {
    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

/// Cluster size of the volume that contains `path` (None if it cannot be determined).
pub fn cluster_size(path: &Path) -> Option<u64> {
    let src = wide(path);
    let mut root = vec![0u16; 1024];
    // SAFETY: `src` is NUL-terminated; `root` is a writable buffer of the stated length.
    let ok = unsafe { GetVolumePathNameW(src.as_ptr(), root.as_mut_ptr(), root.len() as u32) };
    if ok == 0 {
        return None;
    }
    let (mut spc, mut bps, mut free, mut total) = (0u32, 0u32, 0u32, 0u32);
    // SAFETY: `root` was NUL-terminated by GetVolumePathNameW; out-pointers reference live locals.
    let ok = unsafe { GetDiskFreeSpaceW(root.as_ptr(), &mut spc, &mut bps, &mut free, &mut total) };
    (ok != 0 && spc > 0 && bps > 0).then(|| spc as u64 * bps as u64)
}
