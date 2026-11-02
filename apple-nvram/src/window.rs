// SPDX-License-Identifier: MIT
//! A scoped write window for the NVRAM device.
//!
//! The Aurora `spi-apple-qspi` driver keeps the boot NOR read-only until root
//! clears its `read_only` module parameter. A `WriteWindow` holds an exclusive
//! lock on the NVRAM device, clears `read_only` only if it was set, and sets it
//! again when dropped, so the NOR is writable only while one tool is applying
//! its own change. SIGINT, SIGTERM, SIGHUP and SIGQUIT are held off while the
//! window is open, so an interrupted run cannot leave the NOR writable.
//! Where the parameter does not exist (another NVRAM driver, or no QSPI
//! driver loaded) the window only takes the lock.

use std::{
    fs::{self, File},
    io,
    os::unix::io::AsRawFd,
    path::{Path, PathBuf},
};

use nix::sys::signal::{pthread_sigmask, SigSet, SigmaskHow, Signal};

/// The `spi-apple-qspi` runtime opt-in.
pub const QSPI_READ_ONLY: &str = "/sys/module/spi_apple_qspi/parameters/read_only";

pub struct WriteWindow {
    /// The parameter to set back to read-only on drop, if this window cleared it.
    restore: Option<PathBuf>,
    old_mask: Option<SigSet>,
}

fn is_set(value: &str) -> bool {
    matches!(value.trim(), "Y" | "y" | "1")
}

impl WriteWindow {
    /// Lock `device` and open the boot NOR for writing.
    pub fn open(device: &File) -> io::Result<Self> {
        Self::open_with(device, Path::new(QSPI_READ_ONLY))
    }

    /// As `open`, with the read-only parameter at `param`.
    pub fn open_with(device: &File, param: &Path) -> io::Result<Self> {
        #[allow(deprecated)]
        nix::fcntl::flock(device.as_raw_fd(), nix::fcntl::FlockArg::LockExclusive)
            .map_err(io::Error::from)?;

        let value = match fs::read_to_string(param) {
            Ok(value) => value,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(Self { restore: None, old_mask: None });
            }
            Err(e) => return Err(e),
        };
        // An administrator already opted in: leave the setting as found.
        if !is_set(&value) {
            return Ok(Self { restore: None, old_mask: None });
        }

        let mut held = SigSet::empty();
        for sig in [Signal::SIGINT, Signal::SIGTERM, Signal::SIGHUP, Signal::SIGQUIT] {
            held.add(sig);
        }
        let mut old_mask = SigSet::empty();
        pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&held), Some(&mut old_mask))
            .map_err(io::Error::from)?;
        // From here on drop restores both the parameter and the mask.
        let window = Self {
            restore: Some(param.to_owned()),
            old_mask: Some(old_mask),
        };
        fs::write(param, "N").map_err(|e| {
            if e.raw_os_error() == Some(nix::libc::EPERM) {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "the boot NOR refused write access: the QSPI driver latched \
                     read-only after a write fault, or is not ready; reboot \
                     before retrying",
                )
            } else {
                e
            }
        })?;
        Ok(window)
    }
}

impl Drop for WriteWindow {
    fn drop(&mut self) {
        if let Some(param) = self.restore.take() {
            if let Err(e) = fs::write(&param, "Y") {
                eprintln!(
                    "warning: could not set {} back to Y: {}",
                    param.display(),
                    e
                );
            }
        }
        if let Some(mask) = self.old_mask.take() {
            let _ = pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&mask), None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "apple-nvram-window-{}-{}",
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

    fn device(s: &Scratch) -> File {
        let p = s.path("nvram");
        fs::write(&p, [0xffu8; 16]).unwrap();
        File::options().read(true).write(true).open(p).unwrap()
    }

    #[test]
    fn opens_and_restores_read_only() {
        let s = Scratch::new("restore");
        let param = s.path("read_only");
        fs::write(&param, "Y\n").unwrap();
        let dev = device(&s);
        {
            let _w = WriteWindow::open_with(&dev, &param).unwrap();
            assert_eq!(fs::read_to_string(&param).unwrap(), "N");
        }
        assert_eq!(fs::read_to_string(&param).unwrap(), "Y");
    }

    #[test]
    fn restores_after_an_early_return() {
        fn apply(dev: &File, param: &Path) -> io::Result<()> {
            let _w = WriteWindow::open_with(dev, param)?;
            Err(io::Error::new(io::ErrorKind::Other, "write failed"))
        }
        let s = Scratch::new("early");
        let param = s.path("read_only");
        fs::write(&param, "Y\n").unwrap();
        let dev = device(&s);
        assert!(apply(&dev, &param).is_err());
        assert_eq!(fs::read_to_string(&param).unwrap(), "Y");
    }

    #[test]
    fn leaves_an_existing_opt_in_alone() {
        let s = Scratch::new("optin");
        let param = s.path("read_only");
        fs::write(&param, "N\n").unwrap();
        let dev = device(&s);
        drop(WriteWindow::open_with(&dev, &param).unwrap());
        assert_eq!(fs::read_to_string(&param).unwrap(), "N\n");
    }

    #[test]
    fn missing_parameter_only_locks() {
        let s = Scratch::new("missing");
        let param = s.path("absent");
        let dev = device(&s);
        drop(WriteWindow::open_with(&dev, &param).unwrap());
        assert!(!param.exists());
    }

    #[test]
    fn signals_are_held_only_inside_the_window() {
        let s = Scratch::new("mask");
        let param = s.path("read_only");
        fs::write(&param, "Y\n").unwrap();
        let dev = device(&s);
        let held = || SigSet::thread_get_mask().unwrap().contains(Signal::SIGTERM);
        assert!(!held());
        {
            let _w = WriteWindow::open_with(&dev, &param).unwrap();
            assert!(held());
        }
        assert!(!held());
    }
}
