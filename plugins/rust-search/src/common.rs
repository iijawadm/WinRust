// Shared by rs and rs-launcher: drive discovery and the pipe client.
#![allow(dead_code)]
use std::{
    fs::OpenOptions,
    io::{self, Read, Write},
};
use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW};

const DRIVE_FIXED: u32 = 3;

pub fn pipe_name(d: char) -> String {
    format!("\\\\.\\pipe\\rs-search-{d}")
}

/// Letters of all mounted drives. Cheap: no volume is touched, so sleeping disks stay asleep.
pub fn drive_letters() -> Vec<char> {
    let mask = unsafe { GetLogicalDrives() };
    (0..26).filter(|i| mask & (1 << i) != 0).map(|i| (b'A' + i as u8) as char).collect()
}

/// Letters of all fixed local drives (NTFS, exFAT, FAT32) without touching optical or network drives.
pub fn fixed_drives() -> Vec<char> {
    drive_letters()
        .into_iter()
        .filter(|d| {
            let root: Vec<u16> = format!("{d}:\\").encode_utf16().chain([0]).collect();
            unsafe { GetDriveTypeW(root.as_ptr()) == DRIVE_FIXED }
        })
        .collect()
}

/// Fixed (non-removable, non-network) drives formatted as NTFS: the ones the service can index.
pub fn fixed_ntfs_drives() -> Vec<char> {
    drive_letters()
        .into_iter()
        .filter(|d| {
            let root: Vec<u16> = format!("{d}:\\").encode_utf16().chain([0]).collect();
            let mut fs = [0u16; 16];
            let (ty, ok) = unsafe {
                let ty = GetDriveTypeW(root.as_ptr());
                let null = std::ptr::null_mut();
                (ty, GetVolumeInformationW(root.as_ptr(), null, 0, null as _, null as _, null as _, fs.as_mut_ptr(), 16))
            };
            ty == DRIVE_FIXED && ok != 0 && String::from_utf16_lossy(&fs).trim_end_matches('\0') == "NTFS"
        })
        .collect()
}

/// Asks drive `d`'s service. None = no service for that drive (pipe missing).
pub fn ask(d: char, q: &str) -> Option<io::Result<String>> {
    let mut p = match OpenOptions::new().read(true).write(true).open(pipe_name(d)) {
        Ok(p) => p,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(e) => return Some(Err(e)),
    };
    Some((|| {
        p.write_all(q.as_bytes())?;
        let (mut out, mut chunk) = (Vec::new(), [0u8; 1 << 16]);
        loop {
            match p.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&chunk[..n]),
                // the server ends its reply by hanging up: BROKEN_PIPE (109) / NOT_CONNECTED (233) is EOF
                Err(e) if matches!(e.raw_os_error(), Some(109 | 233)) => break,
                Err(e) => return Err(e),
            }
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    })())
}
