// SPDX-License-Identifier: MIT
use std::{
    fs::{File, OpenOptions},
    io::{self, Seek, SeekFrom, Write},
    os::unix::{
        fs::{FileTypeExt, MetadataExt},
        io::AsRawFd,
    },
    path::Path,
};

use crate::NvramWriter;

/// `MTD_WRITEABLE` from `<mtd/mtd-abi.h>`.
pub const MTD_WRITEABLE: u32 = 0x400;

/// Whether `flags` from `MEMGETINFO` and an MTD char-device minor disallow writes.
/// Linux `mtdchar_open` rejects write opens when `MTD_WRITEABLE` is clear or the
/// minor is odd (`/dev/mtdNro`).
pub fn mtd_flags_read_only(flags: u32, minor: u64) -> bool {
    (flags & MTD_WRITEABLE) == 0 || (minor & 1) != 0
}

/// Whether an open file descriptor refers to a read-only Linux MTD device.
/// Returns `false` for regular files and non-MTD devices.
pub fn is_read_only_fd(file: &File) -> bool {
    let mut info = MtdInfoUser::default();
    if unsafe { mtd_mem_get_info(file.as_raw_fd(), &mut info) }.is_err() {
        return false;
    }
    let minor = file
        .metadata()
        .ok()
        .filter(|m| m.file_type().is_char_device())
        .map(|m| nix::sys::stat::minor(m.rdev()))
        .unwrap_or(0);
    mtd_flags_read_only(info.flags, minor)
}

/// Open `path` read-only and report whether it is a read-only MTD device.
/// Returns `false` if `path` cannot be opened or is not an MTD device.
pub fn is_read_only(path: &Path) -> bool {
    File::open(path).map(|f| is_read_only_fd(&f)).unwrap_or(false)
}

/// Open `path` read-write, turning the kernel's `-EACCES` on a read-only MTD
/// partition into an explicit read-only error so callers do not blame missing
/// privileges.
pub fn open_rw(path: &Path) -> io::Result<File> {
    match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => Ok(file),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied && is_read_only(path) => {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "NVRAM device {} is read-only (MTD partition lacks MTD_WRITEABLE)",
                    path.display()
                ),
            ))
        }
        Err(e) => Err(e),
    }
}

impl<T> NvramWriter for T
where
    T: Seek + Write + AsRawFd,
{
    fn erase_if_needed(&mut self, offset: u32, size: usize) -> std::io::Result<()> {
        // Not an MTD device (a file copy, say): there is nothing to erase.
        if unsafe { mtd_mem_get_info(self.as_raw_fd(), &mut MtdInfoUser::default()) }.is_err() {
            return Ok(());
        }
        let erase_info = EraseInfoUser {
            start: offset,
            length: size as u32,
        };
        unsafe { mtd_mem_erase(self.as_raw_fd(), &erase_info) }?;
        Ok(())
    }

    fn write_all(&mut self, offset: u32, buf: &[u8]) -> std::io::Result<()> {
        self.seek(SeekFrom::Start(offset as u64))?;
        self.write_all(buf)?;

        Ok(())
    }
}

#[repr(C)]
pub struct EraseInfoUser {
    start: u32,
    length: u32,
}

#[repr(C)]
#[derive(Default)]
pub struct MtdInfoUser {
    ty: u8,
    flags: u32,
    size: u32,
    erasesize: u32,
    writesize: u32,
    oobsize: u32,
    padding: u64,
}

nix::ioctl_write_ptr!(mtd_mem_erase, b'M', 2, EraseInfoUser);
nix::ioctl_read!(mtd_mem_get_info, b'M', 1, MtdInfoUser);

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "apple-nvram-mtd-{}-{}",
                std::process::id(),
                name
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn mtd_flags_detect_read_only_partitions_and_minors() {
        // Writable NOR (MTD_WRITEABLE | MTD_BIT_WRITEABLE) on even minor (/dev/mtd3).
        assert!(!mtd_flags_read_only(0x0c00, 6));
        // Read-only NOR (MTD_BIT_WRITEABLE only, DT `read-only;`) on even minor.
        assert!(mtd_flags_read_only(0x0800, 6));
        // Writable partition accessed through odd read-only minor (/dev/mtd3ro).
        assert!(mtd_flags_read_only(0x0c00, 7));
        // No flags set at all.
        assert!(mtd_flags_read_only(0, 0));
    }

    #[test]
    fn non_mtd_paths_are_not_reported_read_only() {
        let s = Scratch::new("non-mtd");
        let p = s.path("nvram.bin");
        fs::write(&p, [0xffu8; 16]).unwrap();
        assert!(!is_read_only(&p));
        assert!(!is_read_only(Path::new("/dev/null")));
        assert!(!is_read_only(&s.path("missing")));
    }

    #[test]
    fn open_rw_keeps_os_permission_error_for_non_mtd_files() {
        let s = Scratch::new("perm");
        let p = s.path("nvram.bin");
        fs::write(&p, [0xffu8; 16]).unwrap();
        assert!(open_rw(&p).is_ok());

        // When not running as root, a 0444 regular file fails with OS EACCES,
        // not the MTD read-only message.
        fs::set_permissions(&p, fs::Permissions::from_mode(0o444)).unwrap();
        if let Err(e) = open_rw(&p) {
            assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
            assert!(
                e.raw_os_error().is_some(),
                "non-MTD file should keep its raw OS error: {e}"
            );
        }
    }
}
