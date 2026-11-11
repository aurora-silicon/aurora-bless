// SPDX-License-Identifier: MIT
//! One brightness level for the built-in panel, shared with the boot loader.
//!
//! The boot loader starts the panel at the level in the `system:backlight-nits`
//! NVRAM variable, an ASCII hex 16.16 fixed-point value in nits
//! (`0x008c0000` = 140 nits). The `apple-panel-bl` backlight device also counts
//! in nits. At boot Linux keeps the loader's level (`restore`), and the level
//! the user picks is written back (`watch`, `save`), so the loader, Linux and
//! macOS all start where the user left the panel.
//!
//! NVRAM writes are rate limited: a level is stored once it has been stable
//! for a debounce time, at most once per minimum interval, and once more when
//! the service stops.

use std::{
    borrow::Cow,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use apple_nvram::{nvram_parse, window::WriteWindow, VarType};

/// The variable the boot loader reads, in the system partition.
pub const VARIABLE: &[u8] = b"backlight-nits";
/// The default NVRAM device.
pub const DEVICE: &str = "/dev/mtd/by-name/nvram";
/// The built-in panel's backlight device.
pub const BACKLIGHT: &str = "apple-panel-bl";

/// Restored levels are kept at or above this share of the maximum, so a
/// stored near-zero level can never boot to a dark panel.
pub const RESTORE_FLOOR_PERCENT: u32 = 5;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Nvram(apple_nvram::Error),
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<apple_nvram::Error> for Error {
    fn from(e: apple_nvram::Error) -> Self {
        Error::Nvram(e)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Nvram(e) => write!(f, "NVRAM: {e:?}"),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Whole nits from a `backlight-nits` value, rounded to the nearest nit.
/// Only the ASCII hex 16.16 form is understood; anything else is `None`.
pub fn parse_nits(value: &[u8]) -> Option<u32> {
    let text = std::str::from_utf8(value).ok()?.trim_end_matches('\0').trim();
    let hex = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X"))?;
    if hex.is_empty() || hex.len() > 8 {
        return None;
    }
    let fixed = u32::from_str_radix(hex, 16).ok()?;
    Some(((fixed as u64 + 0x8000) >> 16) as u32)
}

/// The `backlight-nits` value for whole nits.
pub fn format_nits(nits: u32) -> Vec<u8> {
    let nits = nits.min(0xffff);
    format!("0x{:08x}", nits << 16).into_bytes()
}

/// The level to start the panel at for a stored level and the device maximum.
pub fn clamp_restore(nits: u32, max: u32) -> u32 {
    let floor = (max * RESTORE_FLOOR_PERCENT / 100).max(1);
    nits.clamp(floor.min(max), max)
}

/// The level worth storing, or `None` while the panel is blanked or at zero
/// (a screen-off state, not a brightness the user chose).
pub fn savable(level: u32, bl_power: u32) -> Option<u32> {
    (level > 0 && bl_power == 0).then_some(level)
}

/// The stored level in an NVRAM image, if there is one.
pub fn read_nits_from(data: &[u8]) -> Result<Option<u32>> {
    let mut nv = nvram_parse(data)?;
    let part = nv.active_part_mut();
    Ok(part
        .get_variable(VARIABLE, VarType::System)
        .and_then(|v| parse_nits(&v.value())))
}

/// The stored level on an NVRAM device, opened read-only.
pub fn read_nits(device: &Path) -> Result<Option<u32>> {
    let mut data = Vec::new();
    File::open(device)?.read_to_end(&mut data)?;
    read_nits_from(&data)
}

/// Store `nits` on the NVRAM device (or a file copy). Returns false, without
/// writing, when the device already holds that level.
pub fn store_nits(device: &Path, nits: u32) -> Result<bool> {
    let mut file = OpenOptions::new().read(true).write(true).open(device)?;
    // Hold the lock (and the boot NOR write window) from the read to the write.
    let _window = WriteWindow::open(&file)?;
    let mut data = Vec::new();
    file.seek(SeekFrom::Start(0))?;
    file.read_to_end(&mut data)?;
    let value = format_nits(nits);
    let mut nv = nvram_parse(&data)?;
    nv.prepare_for_write();
    let part = nv.active_part_mut();
    if let Some(v) = part.get_variable(VARIABLE, VarType::System) {
        if parse_nits(&v.value()) == Some(nits) {
            return Ok(false);
        }
    }
    part.insert_variable(VARIABLE, Cow::Owned(value), VarType::System);
    nv.apply(&mut file)?;
    Ok(true)
}

/// A backlight class device.
pub struct Backlight {
    dir: PathBuf,
}

impl Backlight {
    pub fn new(name: &str) -> Self {
        Self::at(Path::new("/sys/class/backlight").join(name))
    }

    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn path(&self, attr: &str) -> PathBuf {
        self.dir.join(attr)
    }

    pub fn exists(&self) -> bool {
        self.dir.exists()
    }

    fn read(&self, attr: &str) -> io::Result<u32> {
        fs::read_to_string(self.path(attr))?
            .trim()
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, attr.to_owned()))
    }

    /// The requested level (`actual_brightness` does not follow every panel).
    pub fn level(&self) -> io::Result<u32> {
        self.read("brightness")
    }

    pub fn max(&self) -> io::Result<u32> {
        self.read("max_brightness")
    }

    /// 0 when the panel is on; missing counts as on.
    pub fn power(&self) -> u32 {
        self.read("bl_power").unwrap_or(0)
    }

    pub fn set_level(&self, level: u32) -> io::Result<()> {
        fs::write(self.path("brightness"), level.to_string())
    }
}

/// Decides when a level is written: after `debounce` without a change, and
/// no sooner than `interval` after the previous write.
#[derive(Debug, Clone)]
pub struct Limiter {
    debounce: Duration,
    interval: Duration,
    stored: Option<u32>,
    last_write: Option<Instant>,
    pending: Option<(u32, Instant)>,
}

impl Limiter {
    pub fn new(debounce: Duration, interval: Duration, stored: Option<u32>) -> Self {
        Self {
            debounce,
            interval,
            stored,
            last_write: None,
            pending: None,
        }
    }

    pub fn stored(&self) -> Option<u32> {
        self.stored
    }

    /// The panel is at `level` at `now`.
    pub fn observe(&mut self, level: u32, now: Instant) {
        if Some(level) == self.stored {
            self.pending = None;
        } else if self.pending.map(|(l, _)| l) != Some(level) {
            self.pending = Some((level, now));
        }
    }

    /// When the pending level is due, if one is pending.
    pub fn deadline(&self) -> Option<Instant> {
        let (_, since) = self.pending?;
        let settled = since + self.debounce;
        Some(match self.last_write {
            Some(last) => settled.max(last + self.interval),
            None => settled,
        })
    }

    /// The level to write now, if one is due.
    pub fn due(&self, now: Instant) -> Option<u32> {
        let deadline = self.deadline()?;
        (now >= deadline).then(|| self.pending.unwrap().0)
    }

    /// The level to write when stopping, rate limit aside.
    pub fn flush(&self) -> Option<u32> {
        self.pending.map(|(l, _)| l)
    }

    /// A write of `level` was attempted at `now`; `ok` if it reached NVRAM.
    /// A failed write still waits a full interval before the next try.
    pub fn wrote(&mut self, level: u32, now: Instant, ok: bool) {
        self.last_write = Some(now);
        if ok {
            self.stored = Some(level);
            if self.pending.map(|(l, _)| l) == Some(level) {
                self.pending = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nits_round_trip() {
        assert_eq!(parse_nits(b"0x008c0000"), Some(140));
        assert_eq!(format_nits(140), b"0x008c0000");
        for n in [1, 36, 140, 184, 500, 525, 600, 1600] {
            assert_eq!(parse_nits(&format_nits(n)), Some(n));
        }
        // Fractions round to the nearest nit.
        assert_eq!(parse_nits(b"0x008c8000"), Some(141));
        assert_eq!(parse_nits(b"0x008c7fff"), Some(140));
        assert_eq!(parse_nits(b"0x008c0000\0"), Some(140));
    }

    #[test]
    fn unknown_forms_are_refused() {
        for v in [&b""[..], b"140", b"0x", b"0x1234567890", b"\x00\x00\x8c\x00", b"0xzz"] {
            assert_eq!(parse_nits(v), None, "{v:?}");
        }
    }

    #[test]
    fn restore_clamps() {
        assert_eq!(clamp_restore(140, 525), 140);
        assert_eq!(clamp_restore(0, 525), 26);
        assert_eq!(clamp_restore(1, 600), 30);
        assert_eq!(clamp_restore(9000, 525), 525);
        assert_eq!(clamp_restore(0, 10), 1);
        assert_eq!(clamp_restore(5, 0), 0);
    }

    #[test]
    fn blank_levels_are_not_saved() {
        assert_eq!(savable(140, 0), Some(140));
        assert_eq!(savable(0, 0), None);
        assert_eq!(savable(140, 4), None);
    }

    fn limiter() -> (Limiter, Instant) {
        (
            Limiter::new(Duration::from_secs(5), Duration::from_secs(60), Some(140)),
            Instant::now(),
        )
    }

    #[test]
    fn unchanged_level_is_never_written() {
        let (mut l, t) = limiter();
        l.observe(140, t);
        assert_eq!(l.deadline(), None);
        assert_eq!(l.flush(), None);
    }

    #[test]
    fn a_change_waits_for_the_debounce() {
        let (mut l, t) = limiter();
        l.observe(200, t);
        assert_eq!(l.due(t + Duration::from_secs(4)), None);
        assert_eq!(l.due(t + Duration::from_secs(5)), Some(200));
    }

    #[test]
    fn a_slider_drag_writes_once() {
        let (mut l, t) = limiter();
        // 100 steps 50 ms apart, then the level settles.
        for i in 0..100u32 {
            l.observe(150 + i, t + Duration::from_millis(50 * i as u64));
            assert_eq!(l.due(t + Duration::from_millis(50 * i as u64)), None);
        }
        let end = t + Duration::from_millis(50 * 99);
        assert_eq!(l.deadline(), Some(end + Duration::from_secs(5)));
        assert_eq!(l.due(end + Duration::from_secs(5)), Some(249));
    }

    #[test]
    fn writes_are_spaced_by_the_interval() {
        let (mut l, t) = limiter();
        let mut writes = Vec::new();
        // A new level every 6 s for 10 minutes: each one outlives the debounce.
        let mut now = t;
        let end = t + Duration::from_secs(600);
        let mut level = 141;
        let mut next_change = t;
        while now <= end {
            if now >= next_change {
                l.observe(level, now);
                level += 1;
                next_change += Duration::from_secs(6);
            }
            if let Some(v) = l.due(now) {
                writes.push((now, v));
                l.wrote(v, now, true);
            }
            now += Duration::from_millis(100);
        }
        assert!(writes.len() <= 11, "{} writes", writes.len());
        for w in writes.windows(2) {
            assert!(w[1].0 - w[0].0 >= Duration::from_secs(60));
        }
    }

    #[test]
    fn going_back_to_the_stored_level_cancels() {
        let (mut l, t) = limiter();
        l.observe(200, t);
        l.observe(140, t + Duration::from_secs(1));
        assert_eq!(l.deadline(), None);
    }

    #[test]
    fn stop_flushes_the_pending_level() {
        let (mut l, t) = limiter();
        l.observe(200, t);
        assert_eq!(l.flush(), Some(200));
        l.wrote(200, t, true);
        assert_eq!(l.flush(), None);
        assert_eq!(l.stored(), Some(200));
    }

    #[test]
    fn a_failed_write_backs_off() {
        let (mut l, t) = limiter();
        l.observe(200, t);
        let due = t + Duration::from_secs(5);
        assert_eq!(l.due(due), Some(200));
        l.wrote(200, due, false);
        assert_eq!(l.stored(), Some(140));
        assert_eq!(l.due(due + Duration::from_secs(59)), None);
        assert_eq!(l.due(due + Duration::from_secs(60)), Some(200));
    }

    // An empty v3 store: one bank with a store header, the rest erased.
    #[rustfmt::skip]
    const STORE_HEADER: [u8; 24] = [
        0x33, 0x56, 0x56, 0x4e, 0x00, 0x00, 0x01, 0x00,
        0x01, 0x00, 0x00, 0x00, 0xfe, 0x5a, 0x01, 0x00,
        0x00, 0x40, 0x00, 0x00, 0x00, 0xc0, 0x00, 0x00,
    ];
    const BANK: usize = 0x10000;

    fn image(dir: &Path, banks: usize) -> PathBuf {
        let mut data = vec![0xFF; BANK * banks];
        data[..STORE_HEADER.len()].copy_from_slice(&STORE_HEADER);
        let path = dir.join("nvram.bin");
        fs::write(&path, data).unwrap();
        path
    }

    fn tempdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aurora-panel-nits-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn store_and_read_back() {
        let dir = tempdir("store");
        let nv = image(&dir, 2);
        assert_eq!(read_nits(&nv).unwrap(), None);
        assert!(store_nits(&nv, 140).unwrap());
        assert_eq!(read_nits(&nv).unwrap(), Some(140));
        // The same level again leaves the store untouched.
        let before = fs::read(&nv).unwrap();
        assert!(!store_nits(&nv, 140).unwrap());
        assert_eq!(fs::read(&nv).unwrap(), before);
        assert!(store_nits(&nv, 184).unwrap());
        assert_eq!(read_nits(&nv).unwrap(), Some(184));
        // The value is the loader's ASCII form.
        let data = fs::read(&nv).unwrap();
        assert!(data.windows(10).any(|w| w == b"0x00b80000"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn many_writes_roll_over_banks() {
        let dir = tempdir("roll");
        let nv = image(&dir, 3);
        // Far more records than one bank holds: the store moves to the next bank.
        for i in 0..2000u32 {
            assert!(store_nits(&nv, 100 + (i % 400)).unwrap());
        }
        let last = 100 + (1999 % 400);
        assert_eq!(read_nits(&nv).unwrap(), Some(last));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn backlight_attributes() {
        let dir = tempdir("bl");
        for (k, v) in [("brightness", "161\n"), ("max_brightness", "525\n"), ("bl_power", "0\n")] {
            fs::write(dir.join(k), v).unwrap();
        }
        let bl = Backlight::at(dir.clone());
        assert_eq!(bl.level().unwrap(), 161);
        assert_eq!(bl.max().unwrap(), 525);
        assert_eq!(bl.power(), 0);
        bl.set_level(140).unwrap();
        assert_eq!(bl.level().unwrap(), 140);
        fs::remove_dir_all(dir).unwrap();
    }
}
