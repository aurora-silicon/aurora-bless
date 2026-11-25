// SPDX-License-Identifier: MIT
//! The `preferred-networks` NVRAM variable: the Wi-Fi networks macOS shares
//! with its boot environment (recoveryOS uses them too).

use std::fmt;

/// One record per network.
pub const RECORD_LEN: usize = 0xc0;
const SECURE: std::ops::Range<usize> = 0x08..0x0c;
const SSID_LEN: std::ops::Range<usize> = 0x0c..0x10;
const SSID: usize = 0x10;
/// The WPA passphrase, NUL-padded, and its length. This is what current
/// macOS writes (seen on M1 Pro, M4 Pro and MacBook Neo).
const PASSPHRASE: std::ops::Range<usize> = 0x58..0x98;
const PASSPHRASE_LEN: std::ops::Range<usize> = 0x9c..0xa0;
/// A 256-bit pre-shared key, where the upstream asahi-wifisync read it; only
/// used for a secured record that carries no passphrase.
const PSK: std::ops::Range<usize> = 0xa0..0xc0;
const SSID_MAX: usize = 32;

#[derive(Clone, PartialEq, Eq)]
pub enum Secret {
    /// 8 to 63 printable characters, or 64 hex digits (a raw key).
    Passphrase(String),
    Psk([u8; 32]),
}

/// Never prints the key.
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match self {
            Secret::Passphrase(_) => "Passphrase(<set>)",
            Secret::Psk(_) => "Psk(<set>)",
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Network {
    pub ssid: Vec<u8>,
    /// None for an open network.
    pub psk: Option<Secret>,
}

/// Never prints the key.
impl fmt::Debug for Network {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Network")
            .field("ssid", &String::from_utf8_lossy(&self.ssid))
            .field("psk", &self.psk.as_ref().map(|_| "<set>"))
            .finish()
    }
}

impl Network {
    pub fn ssid_lossy(&self) -> String {
        String::from_utf8_lossy(&self.ssid).into_owned()
    }
}

fn u32_at(rec: &[u8], r: std::ops::Range<usize>) -> u32 {
    u32::from_le_bytes(rec[r].try_into().unwrap())
}

/// A WPA passphrase NetworkManager accepts: 8 to 63 printable ASCII
/// characters, or 64 hex digits.
fn valid_passphrase(p: &[u8]) -> bool {
    match p.len() {
        8..=63 => p.iter().all(|c| (0x20..0x7f).contains(c)),
        64 => p.iter().all(u8::is_ascii_hexdigit),
        _ => false,
    }
}

fn secret(rec: &[u8]) -> Option<Secret> {
    let len = u32_at(rec, PASSPHRASE_LEN) as usize;
    if len > 0 {
        let p = rec[PASSPHRASE].get(..len)?;
        return valid_passphrase(p).then(|| Secret::Passphrase(String::from_utf8(p.to_vec()).unwrap()));
    }
    let psk: [u8; 32] = rec[PSK].try_into().unwrap();
    psk.iter().any(|&b| b != 0).then_some(Secret::Psk(psk))
}

/// Parses the variable. Records that are cut short, empty or carry an SSID
/// longer than 802.11 allows are skipped, and so are secured records whose
/// key is missing or not one NetworkManager could use.
pub fn parse_networks(data: &[u8]) -> Vec<Network> {
    data.chunks_exact(RECORD_LEN)
        .filter_map(|rec| {
            let len = u32_at(rec, SSID_LEN) as usize;
            if len == 0 || len > SSID_MAX {
                return None;
            }
            let ssid = rec[SSID..SSID + len].to_vec();
            if u32_at(rec, SECURE) == 0 {
                return Some(Network { ssid, psk: None });
            }
            Some(Network { ssid, psk: Some(secret(rec)?) })
        })
        .collect()
}

#[cfg(test)]
pub mod fixtures {
    use super::*;

    /// A record as current macOS writes it (flags 0x50004 for WPA).
    pub fn record(ssid: &[u8], pass: Option<&[u8]>) -> Vec<u8> {
        let mut r = vec![0u8; RECORD_LEN];
        let flags: u32 = if pass.is_some() { 0x50004 } else { 0 };
        r[SECURE].copy_from_slice(&flags.to_le_bytes());
        r[SSID_LEN].copy_from_slice(&(ssid.len() as u32).to_le_bytes());
        r[SSID..SSID + ssid.len()].copy_from_slice(ssid);
        if let Some(p) = pass {
            r[PASSPHRASE.start..PASSPHRASE.start + p.len()].copy_from_slice(p);
            r[PASSPHRASE_LEN].copy_from_slice(&(p.len() as u32).to_le_bytes());
        }
        r
    }

    /// A secured record with only a 256-bit key (the upstream layout).
    pub fn psk_record(ssid: &[u8], psk: [u8; 32]) -> Vec<u8> {
        let mut r = record(ssid, None);
        r[SECURE].copy_from_slice(&1u32.to_le_bytes());
        r[PSK].copy_from_slice(&psk);
        r
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{psk_record, record};
    use super::*;

    #[test]
    fn parses_open_and_secure_networks() {
        let mut data = record(b"Home", Some(b"correct horse"));
        data.extend(record(b"Cafe", None));
        data.extend(psk_record(b"Old", [0x11; 32]));
        let nets = parse_networks(&data);
        assert_eq!(nets.len(), 3);
        assert_eq!(nets[0].ssid, b"Home");
        assert_eq!(nets[0].psk, Some(Secret::Passphrase("correct horse".into())));
        assert_eq!(nets[1].ssid, b"Cafe");
        assert_eq!(nets[1].psk, None);
        assert_eq!(nets[2].psk, Some(Secret::Psk([0x11; 32])));
    }

    #[test]
    fn skips_bad_records() {
        let mut data = record(b"", None);
        let mut long = record(b"x", None);
        long[SSID_LEN].copy_from_slice(&200u32.to_le_bytes());
        data.extend(long);
        data.extend(record(b"Ok", None));
        data.extend(record(b"Short", Some(b"1234567"))); // too short for WPA
        data.extend(psk_record(b"Nokey", [0; 32])); // secured, no key
        data.extend([0u8; 0x20]); // cut short
        let nets = parse_networks(&data);
        assert_eq!(nets.len(), 1);
        assert_eq!(nets[0].ssid, b"Ok");
    }

    #[test]
    fn debug_never_shows_the_key() {
        let n = Network { ssid: b"Home".to_vec(), psk: Some(Secret::Passphrase("hunter2hunter2".into())) };
        let s = format!("{n:?}");
        assert!(!s.contains("hunter2"), "{s}");
    }
}
