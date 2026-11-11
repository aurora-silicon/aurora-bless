// SPDX-License-Identifier: MIT
//! Parsers for the Bluetooth pairing records macOS keeps in NVRAM.
//!
//! `BluetoothUHEDevices` holds the adapter address and the BR/EDR devices
//! with their link keys. `BluetoothInfo` holds the LE devices as a list of
//! tag/length/value fields, one device after another, separated by tag 0x0f.

use std::fmt;

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    /// The record ended in the middle of a field.
    Truncated,
    /// The BR/EDR record header is not the one we know.
    BadHeader,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ParseError::Truncated => write!(f, "record is truncated"),
            ParseError::BadHeader => write!(f, "unknown record header"),
        }
    }
}

pub type Result<T> = std::result::Result<T, ParseError>;

pub type Mac = [u8; 6];
pub type Key = [u8; 16];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtDevice {
    pub mac: Mac,
    pub class: u16,
    pub name: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub link_key: Key,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtInfo {
    pub adapter: Mac,
    pub devices: Vec<BtDevice>,
}

/// A long term key as macOS stores it: one flags byte, then the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ltk {
    pub flags: u8,
    pub key: Key,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BleDevice {
    pub mac: Mac,
    /// 0 for a public address, otherwise a static random address.
    pub mac_type: u8,
    pub name: String,
    pub vendor_source: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub appearance: Option<u16>,
    pub irk: Option<Key>,
    /// Tag 0x09: the key the device handed out, used with `ediv`/`rand`.
    pub ltk: Option<Ltk>,
    /// Tag 0x10: a key with no EDIV/Rand (LE Secure Connections).
    pub pltk: Option<Ltk>,
    pub ediv: u16,
    pub rand: u64,
}

struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.0.len() < n {
            return Err(ParseError::Truncated);
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.take(N)?.try_into().unwrap())
    }
}

fn parse_bt_device(c: &mut Cursor) -> Result<BtDevice> {
    let mac = c.array()?;
    let class = c.u16()?;
    c.take(2)?;
    // u16 length, then a NUL-terminated UTF-8 name.
    let name_len = c.u16()? as usize;
    let name = c.take(name_len)?;
    let name = name.strip_suffix(&[0]).unwrap_or(name);
    let name = String::from_utf8_lossy(name).into_owned();
    let link_key = c.array()?;
    let product_id = c.u16()?;
    let vendor_id = c.u16()?;
    c.take(2)?;
    Ok(BtDevice {
        mac,
        class,
        name,
        vendor_id,
        product_id,
        link_key,
    })
}

/// Parses `BluetoothUHEDevices`.
pub fn parse_bt_info(data: &[u8]) -> Result<BtInfo> {
    let mut c = Cursor(data);
    let adapter = c.array()?;
    let count = c.u8()?;
    if c.u8()? != 0x04 {
        return Err(ParseError::BadHeader);
    }
    let devices = (0..count)
        .map(|_| parse_bt_device(&mut c))
        .collect::<Result<_>>()?;
    Ok(BtInfo { adapter, devices })
}

fn le_uint(value: &[u8]) -> Option<u16> {
    match *value {
        [lo] => Some(lo as u16),
        [lo, hi] => Some(u16::from_le_bytes([lo, hi])),
        _ => None,
    }
}

fn ltk(value: &[u8]) -> Option<Ltk> {
    let (&flags, key) = value.split_first()?;
    Some(Ltk {
        flags,
        key: key.try_into().ok()?,
    })
}

/// Parses `BluetoothInfo`. Fields with an unexpected size are skipped with
/// a warning; a field that runs past the end of the record is an error.
pub fn parse_ble_info(data: &[u8]) -> Result<Vec<BleDevice>> {
    let mut devices = Vec::new();
    let mut device = BleDevice::default();
    let mut c = Cursor(data);
    let mut finish = |device: &mut BleDevice| {
        let device = std::mem::take(device);
        // An empty list is stored as a lone separator.
        if device.mac != [0; 6] {
            devices.push(device);
        }
    };
    while !c.0.is_empty() {
        let tag = c.u8()?;
        let len = c.u8()? as usize;
        let value = c.take(len)?;
        let ok = match tag {
            0x02 => {
                device.name = String::from_utf8_lossy(value).into_owned();
                true
            }
            0x04 => le_uint(value).map(|v| device.product_id = v).is_some(),
            0x05 => le_uint(value).map(|v| device.vendor_id = v).is_some(),
            0x06 => value.first().map(|&v| device.vendor_source = v).is_some(),
            0x08 => value.try_into().map(|k| device.irk = Some(k)).is_ok(),
            0x09 => ltk(value).map(|k| device.ltk = Some(k)).is_some(),
            0x0a => value
                .try_into()
                .map(|v| device.ediv = u16::from_le_bytes(v))
                .is_ok(),
            0x0b => value
                .try_into()
                .map(|v| device.rand = u64::from_le_bytes(v))
                .is_ok(),
            0x0e => match value {
                [typ, mac @ ..] if mac.len() == 6 => {
                    device.mac_type = *typ;
                    device.mac.copy_from_slice(mac);
                    true
                }
                _ => false,
            },
            0x0f => {
                finish(&mut device);
                true
            }
            0x10 => ltk(value).map(|k| device.pltk = Some(k)).is_some(),
            0x11 => value
                .try_into()
                .map(|v| device.appearance = Some(u16::from_le_bytes(v)))
                .is_ok(),
            _ => {
                eprintln!("WARNING: unknown BLE pairing field {tag:#04x}, {len} bytes");
                true
            }
        };
        if !ok {
            eprintln!("WARNING: BLE pairing field {tag:#04x} has an unexpected size ({len} bytes)");
        }
    }
    finish(&mut device);
    Ok(devices)
}

#[cfg(test)]
pub(crate) mod fixtures {
    //! Synthetic records. The keys are counting patterns, not real keys.
    use super::*;

    pub const ADAPTER: Mac = [0x02, 0xA0, 0xB0, 0xC0, 0xD0, 0x01];

    pub fn key(seed: u8) -> Key {
        std::array::from_fn(|i| seed.wrapping_add(i as u8))
    }

    pub fn bt_device(mac: Mac, name: &str, link_key: Key) -> BtDevice {
        BtDevice {
            mac,
            class: 0x2540,
            name: name.into(),
            vendor_id: 0x004c,
            product_id: 0x0267,
            link_key,
        }
    }

    pub fn bt_record(adapter: Mac, devices: &[BtDevice]) -> Vec<u8> {
        let mut out = adapter.to_vec();
        out.push(devices.len() as u8);
        out.push(0x04);
        for d in devices {
            out.extend_from_slice(&d.mac);
            out.extend_from_slice(&d.class.to_le_bytes());
            out.extend_from_slice(&[0, 0]);
            let name = [d.name.as_bytes(), &[0]].concat();
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&name);
            out.extend_from_slice(&d.link_key);
            out.extend_from_slice(&d.product_id.to_le_bytes());
            out.extend_from_slice(&d.vendor_id.to_le_bytes());
            out.extend_from_slice(&[0, 0]);
        }
        out
    }

    fn field(out: &mut Vec<u8>, tag: u8, value: &[u8]) {
        out.push(tag);
        out.push(value.len() as u8);
        out.extend_from_slice(value);
    }

    pub fn ble_record(devices: &[BleDevice]) -> Vec<u8> {
        let mut out = Vec::new();
        for (i, d) in devices.iter().enumerate() {
            if i > 0 {
                field(&mut out, 0x0f, &[]);
            }
            field(&mut out, 0x02, d.name.as_bytes());
            field(&mut out, 0x06, &[d.vendor_source]);
            field(&mut out, 0x05, &d.vendor_id.to_le_bytes());
            field(&mut out, 0x04, &d.product_id.to_le_bytes());
            if d.ltk.is_some() {
                field(&mut out, 0x0a, &d.ediv.to_le_bytes());
            }
            if let Some(irk) = d.irk {
                field(&mut out, 0x08, &irk);
            }
            field(&mut out, 0x0e, &[[d.mac_type].as_slice(), &d.mac].concat());
            if let Some(a) = d.appearance {
                field(&mut out, 0x11, &a.to_le_bytes());
            }
            if let Some(k) = d.ltk {
                field(&mut out, 0x09, &[[k.flags].as_slice(), &k.key].concat());
                field(&mut out, 0x0b, &d.rand.to_le_bytes());
            }
            if let Some(k) = d.pltk {
                field(&mut out, 0x10, &[[k.flags].as_slice(), &k.key].concat());
            }
        }
        if devices.is_empty() {
            field(&mut out, 0x0f, &[]);
        }
        out
    }

    /// A mouse paired with legacy pairing: LTK with EDIV and Rand.
    pub fn legacy_mouse() -> BleDevice {
        BleDevice {
            mac: [0xC1, 0x11, 0x22, 0x33, 0x44, 0x55],
            mac_type: 1,
            name: "Test Mouse".into(),
            vendor_source: 2,
            vendor_id: 0x046d,
            product_id: 0xb023,
            appearance: Some(0x03c2),
            irk: Some(key(0x10)),
            ltk: Some(Ltk {
                flags: 0,
                key: key(0x20),
            }),
            pltk: None,
            ediv: 0x1234,
            rand: 0x0102_0304_0506_0708,
        }
    }

    /// A keyboard paired with LE Secure Connections: one key, no EDIV/Rand.
    pub fn sc_keyboard() -> BleDevice {
        BleDevice {
            mac: [0x00, 0x66, 0x77, 0x88, 0x99, 0xAA],
            mac_type: 0,
            name: "Test Keyboard".into(),
            vendor_source: 1,
            vendor_id: 0x004c,
            product_id: 0x029c,
            appearance: None,
            irk: Some(key(0x30)),
            ltk: None,
            pltk: Some(Ltk {
                flags: 1,
                key: key(0x40),
            }),
            ediv: 0,
            rand: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    #[test]
    fn bt_info_round_trips() {
        let devs = vec![
            bt_device([0xAA, 1, 2, 3, 4, 5], "Headphones", key(1)),
            bt_device([0xAA, 1, 2, 3, 4, 6], "Speaker", key(2)),
        ];
        let info = parse_bt_info(&bt_record(ADAPTER, &devs)).unwrap();
        assert_eq!(info.adapter, ADAPTER);
        assert_eq!(info.devices, devs);
    }

    #[test]
    fn bt_info_without_devices() {
        let info = parse_bt_info(&bt_record(ADAPTER, &[])).unwrap();
        assert!(info.devices.is_empty());
    }

    #[test]
    fn bt_info_errors_instead_of_panicking() {
        let rec = bt_record(ADAPTER, &[bt_device([1; 6], "X", key(1))]);
        for len in 0..rec.len() {
            assert!(parse_bt_info(&rec[..len]).is_err(), "len {len}");
        }
        let mut bad = rec.clone();
        bad[7] = 0x05;
        assert_eq!(parse_bt_info(&bad), Err(ParseError::BadHeader));
    }

    #[test]
    fn ble_info_round_trips() {
        let devs = vec![legacy_mouse(), sc_keyboard()];
        assert_eq!(parse_ble_info(&ble_record(&devs)).unwrap(), devs);
    }

    #[test]
    fn appearance_is_parsed() {
        let devs = parse_ble_info(&ble_record(&[legacy_mouse()])).unwrap();
        assert_eq!(devs[0].appearance, Some(0x03c2));
    }

    #[test]
    fn a_lone_separator_is_no_device() {
        assert!(parse_ble_info(&[0x0f, 0x00]).unwrap().is_empty());
        assert!(parse_ble_info(&[]).unwrap().is_empty());
    }

    #[test]
    fn ble_info_errors_instead_of_panicking() {
        let rec = ble_record(&[legacy_mouse()]);
        // Cutting inside a field is an error; cutting between fields is not.
        for len in 0..rec.len() {
            let _ = parse_ble_info(&rec[..len]);
        }
        assert_eq!(parse_ble_info(&[0x02, 0x05, b'a']), Err(ParseError::Truncated));
    }

    #[test]
    fn odd_sized_fields_are_skipped() {
        let mut rec = ble_record(&[legacy_mouse()]);
        rec.extend_from_slice(&[0x0a, 0x03, 1, 2, 3, 0x08, 0x02, 1, 2]);
        let devs = parse_ble_info(&rec).unwrap();
        assert_eq!(devs[0].ediv, 0x1234);
        assert_eq!(devs[0].irk, Some(key(0x10)));
    }
}
