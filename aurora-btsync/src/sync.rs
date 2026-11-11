// SPDX-License-Identifier: MIT
//! Writes the pairings found in NVRAM into BlueZ's storage.
//!
//! A device with no BlueZ `info` file gets a new one. A device that already
//! has one gets its key groups replaced and everything else (services, name,
//! appearance, connection parameters) kept, but only when macOS changed the
//! keys since the last sync: the state file records a fingerprint of the
//! NVRAM keys last applied, so a pairing made later on Linux is not undone
//! by older keys still sitting in NVRAM. Devices that are not in NVRAM are
//! never touched.

use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

use crate::keyfile::{escape, KeyFile};
use crate::parse::{BleDevice, BtDevice, BtInfo, Key, Mac};

pub fn format_mac(mac: &Mac) -> String {
    mac.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// BR/EDR link keys are stored byte-reversed relative to NVRAM.
fn link_key_hex(key: &Key) -> String {
    key.iter().rev().map(|b| format!("{b:02X}")).collect()
}

/// LE keys are stored in NVRAM byte order.
fn le_key_hex(key: &Key) -> String {
    key.iter().map(|b| format!("{b:02X}")).collect()
}

/// BlueZ's `Authenticated` value is the mgmt LTK type.
const LTK_UNAUTHENTICATED: u8 = 0x00;
const LTK_P256_UNAUTH: u8 = 0x02;

/// The key groups this tool owns in an LE device's `info` file.
const LE_KEY_GROUPS: [&str; 4] = [
    "IdentityResolvingKey",
    "LongTermKey",
    "PeripheralLongTermKey",
    "SlaveLongTermKey",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    BrEdr,
    Le,
}

impl Transport {
    pub fn label(self) -> &'static str {
        match self {
            Transport::BrEdr => "BR/EDR",
            Transport::Le => "LE",
        }
    }
}

pub trait Device {
    fn mac(&self) -> Mac;
    fn transport(&self) -> Transport;
    /// The `info` file for a device BlueZ has never seen.
    fn render_new(&self) -> KeyFile;
    /// Replaces the key material in an existing `info` file.
    fn apply_keys(&self, kf: &mut KeyFile);
}

fn set_ltk(kf: &mut KeyFile, group: &str, key: &Key, auth: u8, ediv: u16, rand: u64) {
    kf.set(group, "Key", &le_key_hex(key));
    kf.set(group, "Authenticated", &auth.to_string());
    kf.set(group, "EncSize", "16");
    kf.set(group, "EDiv", &ediv.to_string());
    kf.set(group, "Rand", &rand.to_string());
}

impl Device for BtDevice {
    fn mac(&self) -> Mac {
        self.mac
    }
    fn transport(&self) -> Transport {
        Transport::BrEdr
    }
    fn render_new(&self) -> KeyFile {
        let mut kf = KeyFile::default();
        kf.set("General", "Name", &escape(&self.name));
        kf.set("General", "Class", &format!("0x{:06x}", self.class));
        kf.set("General", "SupportedTechnologies", "BR/EDR;");
        kf.set("General", "Trusted", "true");
        kf.set("General", "Blocked", "false");
        kf.set("General", "WakeAllowed", "true");
        self.apply_keys(&mut kf);
        kf.set("DeviceID", "Vendor", &self.vendor_id.to_string());
        kf.set("DeviceID", "Product", &self.product_id.to_string());
        kf
    }
    fn apply_keys(&self, kf: &mut KeyFile) {
        // Type and PINLength describe how the device pairs, which a
        // re-pair does not change, so BlueZ's values are kept.
        kf.set("LinkKey", "Key", &link_key_hex(&self.link_key));
    }
}

impl Device for BleDevice {
    fn mac(&self) -> Mac {
        self.mac
    }
    fn transport(&self) -> Transport {
        Transport::Le
    }
    fn render_new(&self) -> KeyFile {
        let mut kf = KeyFile::default();
        kf.set("General", "Name", &escape(&self.name));
        if let Some(a) = self.appearance {
            kf.set("General", "Appearance", &format!("0x{a:04x}"));
        }
        let addr_type = if self.mac_type == 0 { "public" } else { "static" };
        kf.set("General", "AddressType", addr_type);
        kf.set("General", "SupportedTechnologies", "LE;");
        kf.set("General", "Trusted", "true");
        kf.set("General", "Blocked", "false");
        kf.set("General", "CablePairing", "false");
        kf.set("General", "WakeAllowed", "true");
        kf.set("ConnectionParameters", "MinInterval", "6");
        kf.set("ConnectionParameters", "MaxInterval", "6");
        kf.set("ConnectionParameters", "Latency", "0");
        kf.set("ConnectionParameters", "Timeout", "300");
        self.apply_keys(&mut kf);
        kf.set("DeviceID", "Source", &self.vendor_source.to_string());
        kf.set("DeviceID", "Vendor", &self.vendor_id.to_string());
        kf.set("DeviceID", "Product", &self.product_id.to_string());
        kf.set("DeviceID", "Version", "0");
        kf
    }
    fn apply_keys(&self, kf: &mut KeyFile) {
        for g in LE_KEY_GROUPS {
            kf.remove_group(g);
        }
        if let Some(irk) = self.irk.filter(|k| *k != [0; 16]) {
            kf.set("IdentityResolvingKey", "Key", &le_key_hex(&irk));
        }
        let ltk = self.ltk.filter(|k| k.key != [0; 16]);
        let pltk = self.pltk.filter(|k| k.key != [0; 16]);
        // [LongTermKey] is the key BlueZ starts encryption with when Linux
        // is the central, which is the normal role with a mouse, keyboard
        // or trackpad; without it the device connects but HID never comes
        // up. With legacy pairing that is the key the device handed out,
        // with its EDIV and Rand. With LE Secure Connections there is one
        // key for both roles and no EDIV/Rand.
        let legacy = ltk.filter(|_| self.ediv != 0 || self.rand != 0);
        let sc = pltk.or(ltk.filter(|_| legacy.is_none()));
        if let Some(k) = legacy {
            set_ltk(kf, "LongTermKey", &k.key, LTK_UNAUTHENTICATED, self.ediv, self.rand);
        } else if let Some(k) = sc {
            set_ltk(kf, "LongTermKey", &k.key, LTK_P256_UNAUTH, 0, 0);
        }
        // The key used when the device is the central. BlueZ reads either
        // name; both are written, as BlueZ does itself.
        if let Some(k) = sc {
            for g in ["PeripheralLongTermKey", "SlaveLongTermKey"] {
                set_ltk(kf, g, &k.key, LTK_P256_UNAUTH, 0, 0);
            }
        }
    }
}

/// A fingerprint of the key material NVRAM holds for a device (FNV-1a over
/// the rendered key groups). Only used to notice that macOS changed them.
fn fingerprint(dev: &dyn Device) -> String {
    let mut kf = KeyFile::default();
    dev.apply_keys(&mut kf);
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in kf.to_text().bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// `<adapter>/<device> <fingerprint>` lines.
#[derive(Debug, Default)]
pub struct State {
    path: Option<PathBuf>,
    entries: BTreeMap<String, String>,
}

impl State {
    pub fn load(path: &Path) -> io::Result<State> {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        let entries = text
            .lines()
            .filter_map(|l| l.split_once(' '))
            .map(|(k, v)| (k.to_owned(), v.trim().to_owned()))
            .collect();
        Ok(State {
            path: Some(path.to_owned()),
            entries,
        })
    }

    fn save(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let text: String = self
            .entries
            .iter()
            .map(|(k, v)| format!("{k} {v}\n"))
            .collect();
        if let Some(dir) = path.parent() {
            fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        }
        write_atomic(path, &text)
    }
}

fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    let tmp = path.with_extension("aurora-btsync.tmp");
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    fs::rename(&tmp, path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// No `info` file existed; one was written.
    Added,
    /// macOS changed the keys; the key groups were replaced.
    Updated,
    /// Nothing to do.
    Unchanged,
}

pub fn sync_device(
    adapter_dir: &Path,
    adapter: &Mac,
    dev: &dyn Device,
    state: &mut State,
) -> io::Result<Outcome> {
    let mac = format_mac(&dev.mac());
    let dev_dir = adapter_dir.join(&mac);
    let info = dev_dir.join("info");
    let state_key = format!("{}/{}", format_mac(adapter), mac);
    let fp = fingerprint(dev);

    let outcome = match fs::read_to_string(&info) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&dev_dir)?;
            write_atomic(&info, &dev.render_new().to_text())?;
            Outcome::Added
        }
        Err(e) => return Err(e),
        Ok(_) if state.entries.get(&state_key) == Some(&fp) => Outcome::Unchanged,
        Ok(text) => {
            let mut kf = KeyFile::parse(&text);
            dev.apply_keys(&mut kf);
            let new = kf.to_text();
            if new == text {
                Outcome::Unchanged
            } else {
                write_atomic(&info, &new)?;
                Outcome::Updated
            }
        }
    };
    state.entries.insert(state_key, fp);
    Ok(outcome)
}

/// What sync needs from bluetoothd, behind a trait so it can be tested.
pub trait Bluez {
    /// Whether bluetooth.service is running (or starting).
    fn running(&self) -> bool;
    /// Restarts bluetoothd so it loads the new files. With `wait`, returns
    /// once the adapter is back; without, only queues the restart.
    fn restart(&self, adapter: &Mac, wait: bool) -> Result<(), String>;
    fn connect(&self, adapter: &Mac, dev: &Mac) -> Result<(), String>;
}

pub struct Options {
    /// The storage directory is bluetoothd's own, so bluetoothd has to be
    /// told about changes. False when syncing into another directory.
    pub notify_bluetoothd: bool,
    /// bluetoothd restarts after this process exits (we run as a unit that
    /// bluetooth.service is ordered after), so do not wait or connect.
    pub restart_after_exit: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub changed: Vec<(Mac, Transport, Outcome)>,
    pub connect_failures: usize,
}

pub fn sync(
    config: &Path,
    bt: &BtInfo,
    ble: &[BleDevice],
    state: &mut State,
    bluez: &dyn Bluez,
    opts: &Options,
) -> Result<Report, String> {
    let adapter_dir = config.join(format_mac(&bt.adapter));
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&adapter_dir)
        .map_err(|e| format!("{}: {e}", adapter_dir.display()))?;

    let devices = bt
        .devices
        .iter()
        .map(|d| d as &dyn Device)
        .chain(ble.iter().map(|d| d as &dyn Device));
    let mut report = Report::default();
    for dev in devices {
        let mac = format_mac(&dev.mac());
        match sync_device(&adapter_dir, &bt.adapter, dev, state) {
            Ok(Outcome::Unchanged) => {}
            Ok(o) => {
                let what = if o == Outcome::Added { "added" } else { "updated" };
                println!("{mac} {} {what}", dev.transport().label());
                report.changed.push((dev.mac(), dev.transport(), o));
            }
            Err(e) => eprintln!("{mac}: {e}"),
        }
    }
    state.save().map_err(|e| format!("state file: {e}"))?;

    if report.changed.is_empty() || !opts.notify_bluetoothd {
        return Ok(report);
    }
    if !bluez.running() {
        println!("bluetoothd is not running; it loads the keys when it starts");
        return Ok(report);
    }
    if opts.restart_after_exit {
        bluez.restart(&bt.adapter, false)?;
        println!("bluetoothd restarts after this sync to load the keys");
        return Ok(report);
    }
    bluez.restart(&bt.adapter, true)?;
    // Only the devices whose keys changed are connected. A device that is
    // off or out of range just times out; that is not a sync failure.
    for (mac, _, _) in &report.changed {
        if let Err(e) = bluez.connect(&bt.adapter, mac) {
            eprintln!("{}: connect failed: {e}", format_mac(mac));
            report.connect_failures += 1;
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::fixtures::*;
    use crate::parse::{parse_ble_info, parse_bt_info};
    use std::cell::RefCell;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir = std::env::temp_dir()
                .join(format!("aurora-btsync-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
        fn info(&self, mac: &Mac) -> PathBuf {
            self.0.join(format_mac(&ADAPTER)).join(format_mac(mac)).join("info")
        }
        fn state(&self) -> State {
            State::load(&self.0.join("state/devices")).unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Default)]
    struct FakeBluez {
        running: bool,
        fail_connect: Vec<Mac>,
        log: RefCell<Vec<String>>,
    }

    impl Bluez for FakeBluez {
        fn running(&self) -> bool {
            self.running
        }
        fn restart(&self, _: &Mac, wait: bool) -> Result<(), String> {
            self.log.borrow_mut().push(format!("restart wait={wait}"));
            Ok(())
        }
        fn connect(&self, _: &Mac, dev: &Mac) -> Result<(), String> {
            self.log.borrow_mut().push(format!("connect {}", format_mac(dev)));
            if self.fail_connect.contains(dev) {
                Err("org.bluez.Error.Failed: br-connection-page-timeout".into())
            } else {
                Ok(())
            }
        }
    }

    const NOW: Options = Options {
        notify_bluetoothd: true,
        restart_after_exit: false,
    };

    fn headphones() -> BtDevice {
        bt_device([0xAC, 0x80, 0x0A, 0x01, 0x02, 0x03], "Headphones", key(0x50))
    }

    /// Parses synthetic NVRAM records, as main does with the real ones.
    fn nvram(bt: &[BtDevice], ble: &[BleDevice]) -> (BtInfo, Vec<BleDevice>) {
        (
            parse_bt_info(&bt_record(ADAPTER, bt)).unwrap(),
            parse_ble_info(&ble_record(ble)).unwrap(),
        )
    }

    fn run(s: &Scratch, bt: &BtInfo, ble: &[BleDevice], bluez: &FakeBluez) -> Report {
        let mut state = s.state();
        sync(&s.0, bt, ble, &mut state, bluez, &NOW).unwrap()
    }

    #[test]
    fn new_devices_get_info_files() {
        let s = Scratch::new("new");
        let (bt, ble) = nvram(&[headphones()], &[legacy_mouse(), sc_keyboard()]);
        let bluez = FakeBluez {
            running: true,
            ..Default::default()
        };
        let r = run(&s, &bt, &ble, &bluez);
        assert_eq!(r.changed.len(), 3);

        let hp = fs::read_to_string(s.info(&headphones().mac)).unwrap();
        assert!(hp.contains("[LinkKey]\nKey=5F5E5D5C5B5A59585756555453525150\n"), "{hp}");
        assert!(hp.contains("Class=0x002540\n"));
        assert!(!hp.contains("SelficeID"));

        let mouse = fs::read_to_string(s.info(&legacy_mouse().mac)).unwrap();
        assert!(mouse.contains("Appearance=0x03c2\n"));
        assert!(mouse.contains("AddressType=static\n"));
        assert!(mouse.contains(
            "[LongTermKey]\nKey=202122232425262728292A2B2C2D2E2F\nAuthenticated=0\nEncSize=16\n\
             EDiv=4660\nRand=72623859790382856\n"
        ), "{mouse}");
        // Legacy pairing: the key Linux would hand out as peripheral is not in NVRAM.
        assert!(!mouse.contains("PeripheralLongTermKey"));
        assert!(mouse.contains("[IdentityResolvingKey]\nKey=101112131415161718191A1B1C1D1E1F\n"));

        let kbd = fs::read_to_string(s.info(&sc_keyboard().mac)).unwrap();
        let sc = "Key=404142434445464748494A4B4C4D4E4F\nAuthenticated=2\nEncSize=16\nEDiv=0\nRand=0\n";
        for g in ["LongTermKey", "PeripheralLongTermKey", "SlaveLongTermKey"] {
            assert!(kbd.contains(&format!("[{g}]\n{sc}")), "{g}: {kbd}");
        }
        assert!(kbd.contains("AddressType=public\n"));

        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(s.info(&sc_keyboard().mac)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);

        let log = bluez.log.borrow();
        assert_eq!(log[0], "restart wait=true");
        assert_eq!(log.len(), 4);
    }

    #[test]
    fn a_second_run_changes_nothing() {
        let s = Scratch::new("idem");
        let (bt, ble) = nvram(&[headphones()], &[legacy_mouse()]);
        let bluez = FakeBluez {
            running: true,
            ..Default::default()
        };
        run(&s, &bt, &ble, &bluez);
        let before = fs::read_to_string(s.info(&legacy_mouse().mac)).unwrap();
        bluez.log.borrow_mut().clear();
        let r = run(&s, &bt, &ble, &bluez);
        assert!(r.changed.is_empty());
        assert!(bluez.log.borrow().is_empty(), "no restart, no connect");
        assert_eq!(fs::read_to_string(s.info(&legacy_mouse().mac)).unwrap(), before);
    }

    /// The device is already known to BlueZ (older sync, or paired on Linux
    /// before), then macOS re-pairs it: new keys, BlueZ's fields kept.
    #[test]
    fn existing_info_gets_new_keys_and_keeps_bluez_fields() {
        let s = Scratch::new("update");
        let mouse = legacy_mouse();
        let info = s.info(&mouse.mac);
        fs::create_dir_all(info.parent().unwrap()).unwrap();
        let bluez_file = "[General]\nName=Test Mouse\nAppearance=0x03c2\nAddressType=static\n\
SupportedTechnologies=LE;\nTrusted=true\nBlocked=false\nWakeAllowed=true\n\
Services=00001800-0000-1000-8000-00805f9b34fb;00001812-0000-1000-8000-00805f9b34fb;\n\n\
[IdentityResolvingKey]\nKey=FFEEDDCCBBAA99887766554433221100\n\n\
[PeripheralLongTermKey]\nKey=00000000000000000000000000000001\nAuthenticated=2\nEncSize=16\nEDiv=0\nRand=0\n\n\
[DeviceID]\nSource=2\nVendor=1133\nProduct=45091\nVersion=17\n";
        fs::write(&info, bluez_file).unwrap();

        let (bt, ble) = nvram(&[], &[mouse.clone()]);
        let bluez = FakeBluez {
            running: true,
            ..Default::default()
        };
        let r = run(&s, &bt, &ble, &bluez);
        assert_eq!(r.changed, vec![(mouse.mac, Transport::Le, Outcome::Updated)]);

        let new = fs::read_to_string(&info).unwrap();
        assert!(new.contains("Services=00001800-0000-1000-8000-00805f9b34fb;00001812"));
        assert!(new.contains("[DeviceID]\nSource=2\nVendor=1133\nProduct=45091\nVersion=17\n"));
        assert!(new.contains("[IdentityResolvingKey]\nKey=101112131415161718191A1B1C1D1E1F\n"));
        assert!(new.contains("[LongTermKey]\nKey=202122232425262728292A2B2C2D2E2F\n"));
        assert!(!new.contains("FFEEDDCC"));
        assert!(!new.contains("PeripheralLongTermKey"), "stale key group removed: {new}");
        assert_eq!(*bluez.log.borrow(), vec![
            "restart wait=true".to_string(),
            format!("connect {}", format_mac(&mouse.mac)),
        ]);
    }

    #[test]
    fn br_edr_refresh_keeps_link_key_type() {
        let s = Scratch::new("bredr");
        let hp = headphones();
        let info = s.info(&hp.mac);
        fs::create_dir_all(info.parent().unwrap()).unwrap();
        fs::write(&info, "[General]\nName=Headphones\nServices=0000110b-0000-1000-8000-00805f9b34fb;\n\n\
[LinkKey]\nKey=00000000000000000000000000000000\nType=4\nPINLength=0\n").unwrap();
        let (bt, ble) = nvram(&[hp.clone()], &[]);
        run(&s, &bt, &ble, &FakeBluez::default());
        let new = fs::read_to_string(&info).unwrap();
        assert!(new.contains("[LinkKey]\nKey=5F5E5D5C5B5A59585756555453525150\nType=4\nPINLength=0\n"), "{new}");
        assert!(new.contains("Services=0000110b"));
    }

    /// Pairings that exist only on Linux are never touched.
    #[test]
    fn linux_only_pairings_are_left_alone() {
        let s = Scratch::new("linuxonly");
        let other: Mac = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
        let info = s.info(&other);
        fs::create_dir_all(info.parent().unwrap()).unwrap();
        let text = "[General]\nName=Linux Only\n\n[LinkKey]\nKey=0123\n";
        fs::write(&info, text).unwrap();
        let (bt, ble) = nvram(&[headphones()], &[sc_keyboard()]);
        run(&s, &bt, &ble, &FakeBluez::default());
        assert_eq!(fs::read_to_string(&info).unwrap(), text);
    }

    /// After a sync, the device is re-paired on Linux. NVRAM still holds the
    /// keys macOS had, so the Linux keys must stay.
    #[test]
    fn a_later_linux_pairing_is_not_undone() {
        let s = Scratch::new("relinux");
        let (bt, ble) = nvram(&[], &[sc_keyboard()]);
        run(&s, &bt, &ble, &FakeBluez::default());
        let info = s.info(&sc_keyboard().mac);
        let relinux = fs::read_to_string(&info)
            .unwrap()
            .replace("404142434445464748494A4B4C4D4E4F", "99999999999999999999999999999999");
        fs::write(&info, &relinux).unwrap();
        let r = run(&s, &bt, &ble, &FakeBluez::default());
        assert!(r.changed.is_empty());
        assert_eq!(fs::read_to_string(&info).unwrap(), relinux);

        // Then macOS re-pairs it: the new NVRAM keys win again.
        let mut kbd = sc_keyboard();
        kbd.pltk.as_mut().unwrap().key = key(0x70);
        let (bt, ble) = nvram(&[], &[kbd]);
        let r = run(&s, &bt, &ble, &FakeBluez::default());
        assert_eq!(r.changed.len(), 1);
        assert!(fs::read_to_string(&info).unwrap().contains("Key=707172737475767778797A7B7C7D7E7F"));
    }

    #[test]
    fn connect_timeouts_are_logged_and_ignored() {
        let s = Scratch::new("timeout");
        let hp = headphones();
        let (bt, ble) = nvram(&[hp.clone()], &[legacy_mouse()]);
        let bluez = FakeBluez {
            running: true,
            fail_connect: vec![hp.mac],
            ..Default::default()
        };
        let r = run(&s, &bt, &ble, &bluez);
        assert_eq!(r.connect_failures, 1);
        assert_eq!(r.changed.len(), 2);
        // The mouse after the failing headphones is still connected.
        assert_eq!(bluez.log.borrow().last().unwrap(), &format!("connect {}", format_mac(&legacy_mouse().mac)));
        assert!(s.info(&legacy_mouse().mac).exists());
    }

    #[test]
    fn bluetoothd_not_running_means_no_restart() {
        let s = Scratch::new("stopped");
        let (bt, ble) = nvram(&[headphones()], &[]);
        let bluez = FakeBluez::default();
        let r = run(&s, &bt, &ble, &bluez);
        assert_eq!(r.changed.len(), 1);
        assert!(bluez.log.borrow().is_empty());
    }

    #[test]
    fn another_directory_leaves_bluetoothd_alone() {
        let s = Scratch::new("otherdir");
        let (bt, ble) = nvram(&[headphones()], &[]);
        let bluez = FakeBluez {
            running: true,
            ..Default::default()
        };
        let mut state = s.state();
        let opts = Options {
            notify_bluetoothd: false,
            restart_after_exit: false,
        };
        let r = sync(&s.0, &bt, &ble, &mut state, &bluez, &opts).unwrap();
        assert_eq!(r.changed.len(), 1);
        assert!(bluez.log.borrow().is_empty());
    }

    #[test]
    fn inside_the_unit_the_restart_is_only_queued() {
        let s = Scratch::new("unit");
        let (bt, ble) = nvram(&[headphones()], &[]);
        let bluez = FakeBluez {
            running: true,
            ..Default::default()
        };
        let mut state = s.state();
        let opts = Options {
            notify_bluetoothd: true,
            restart_after_exit: true,
        };
        sync(&s.0, &bt, &ble, &mut state, &bluez, &opts).unwrap();
        assert_eq!(*bluez.log.borrow(), vec!["restart wait=false".to_string()]);
    }
}
