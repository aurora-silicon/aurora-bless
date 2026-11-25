// SPDX-License-Identifier: MIT
//! NetworkManager keyfiles for the networks saved in macOS.
//!
//! Each network gets `aurora-macos-<ssid in hex>.nmconnection` in NM's
//! system-connections directory (root, 0600, as NM requires). A network
//! the user already set up on Linux (any other keyfile with the same SSID)
//! is left alone. A file written before only gets its key refreshed, so
//! settings changed on Linux (autoconnect, IP, priority) are kept.

use std::{
    fs,
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

use crate::parse::{Network, Secret};

pub const PREFIX: &str = "aurora-macos-";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Added,
    Updated,
    Unchanged,
    /// A profile made on Linux has this SSID.
    KeptLinuxProfile,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn file_name(ssid: &[u8]) -> String {
    format!("{PREFIX}{}.nmconnection", hex(ssid))
}

/// The SSID as a keyfile value: plain text when it is safe as one, else the
/// byte list form NM also reads (`72;111;109;101;`).
pub fn ssid_value(ssid: &[u8]) -> String {
    let plain = !ssid.is_empty()
        && ssid.iter().all(|&c| (0x21..0x7f).contains(&c) && c != b';' && c != b'\\' && c != b'#');
    if plain {
        String::from_utf8(ssid.to_vec()).unwrap()
    } else {
        ssid.iter().map(|b| format!("{b};")).collect()
    }
}

fn parse_ssid_value(v: &str) -> Vec<u8> {
    let list: Option<Vec<u8>> = v
        .strip_suffix(';')
        .filter(|s| !s.is_empty())
        .and_then(|s| s.split(';').map(|p| p.trim().parse::<u8>().ok()).collect());
    list.unwrap_or_else(|| v.as_bytes().to_vec())
}

/// A stable UUID (RFC 4122 layout, random version) from the SSID, so the
/// same network gets the same connection on every sync.
pub fn uuid(ssid: &[u8]) -> String {
    let fnv = |seed: u64| {
        ssid.iter().fold(seed, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3))
    };
    let (a, b) = (fnv(0xcbf29ce484222325), fnv(0x84222325cbf29ce4));
    let mut u = [0u8; 16];
    u[..8].copy_from_slice(&a.to_be_bytes());
    u[8..].copy_from_slice(&b.to_be_bytes());
    u[6] = (u[6] & 0x0f) | 0x40;
    u[8] = (u[8] & 0x3f) | 0x80;
    let h = hex(&u);
    format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

pub fn render(net: &Network) -> String {
    let mut s = format!(
        "[connection]\nid={}\nuuid={}\ntype=wifi\n\n[wifi]\nmode=infrastructure\nssid={}\n\n",
        net.ssid_lossy().replace(['\n', '\r'], " "),
        uuid(&net.ssid),
        ssid_value(&net.ssid)
    );
    if let Some(psk) = &net.psk {
        s += &format!("[wifi-security]\nkey-mgmt=wpa-psk\npsk={}\n\n", psk_value(psk));
    }
    s += "[ipv4]\nmethod=auto\n\n[ipv6]\nmethod=auto\n";
    s
}

/// The keyfile value of the key: the passphrase with the keyfile escapes
/// for backslash and spaces, or a 256-bit key as 64 hex digits.
fn psk_value(s: &Secret) -> String {
    match s {
        Secret::Passphrase(p) => p.replace('\\', "\\\\").replace(' ', "\\s"),
        Secret::Psk(k) => hex(k),
    }
}

/// The `[wifi] ssid` of a keyfile, if it is a Wi-Fi profile.
fn keyfile_ssid(text: &str) -> Option<Vec<u8>> {
    let mut group = "";
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            group = line;
        } else if group == "[wifi]" {
            if let Some(v) = line.strip_prefix("ssid=") {
                return Some(parse_ssid_value(v));
            }
        }
    }
    None
}

/// The SSIDs of the Wi-Fi profiles in `dir` that this tool did not write.
pub fn linux_ssids(dir: &Path) -> io::Result<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir)? {
        let e = e?;
        let name = e.file_name();
        if name.to_string_lossy().starts_with(PREFIX) || !e.file_type()?.is_file() {
            continue;
        }
        if let Some(s) = fs::read_to_string(e.path()).ok().as_deref().and_then(keyfile_ssid) {
            out.push(s);
        }
    }
    Ok(out)
}

/// Replaces the key of a file written earlier, keeping everything else.
fn refresh_key(text: &str, net: &Network) -> String {
    let fresh = render(net);
    let Some(psk) = &net.psk else { return text.to_string() };
    let want = format!("psk={}", psk_value(psk));
    let mut group = "";
    let mut found = false;
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        if line.trim().starts_with('[') {
            group = line.trim();
        }
        if group == "[wifi-security]" && line.trim().starts_with("psk=") {
            out.push(want.clone());
            found = true;
        } else {
            out.push(line.to_string());
        }
    }
    if !found {
        // Became a secured network: take the fresh file.
        return fresh;
    }
    out.join("\n") + "\n"
}

fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    let tmp = path.with_extension("aurora-wifisync.tmp");
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

pub fn sync_network(dir: &Path, net: &Network, linux: &[Vec<u8>]) -> io::Result<Outcome> {
    if linux.iter().any(|s| s == &net.ssid) {
        return Ok(Outcome::KeptLinuxProfile);
    }
    let path: PathBuf = dir.join(file_name(&net.ssid));
    match fs::read_to_string(&path) {
        Ok(old) => {
            let new = refresh_key(&old, net);
            if new == old {
                return Ok(Outcome::Unchanged);
            }
            write_atomic(&path, &new)?;
            Ok(Outcome::Updated)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            write_atomic(&path, &render(net))?;
            Ok(Outcome::Added)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(name: &str) -> Scratch {
            let d = std::env::temp_dir().join(format!("aurora-wifisync-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&d);
            fs::create_dir_all(&d).unwrap();
            Scratch(d)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn home() -> Network {
        Network { ssid: b"Home".to_vec(), psk: Some(Secret::Passphrase("correct horse".into())) }
    }

    #[test]
    fn ssid_forms() {
        assert_eq!(ssid_value(b"Home"), "Home");
        assert_eq!(ssid_value(b"My Net"), "77;121;32;78;101;116;");
        assert_eq!(parse_ssid_value("77;121;32;78;101;116;"), b"My Net");
        assert_eq!(parse_ssid_value("Home"), b"Home");
    }

    #[test]
    fn uuid_is_stable_and_well_formed() {
        let u = uuid(b"Home");
        assert_eq!(u, uuid(b"Home"));
        assert_ne!(u, uuid(b"Cafe"));
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
    }

    #[test]
    fn new_network_gets_a_private_keyfile() {
        let s = Scratch::new("new");
        assert_eq!(sync_network(&s.0, &home(), &[]).unwrap(), Outcome::Added);
        let p = s.0.join(file_name(b"Home"));
        let t = fs::read_to_string(&p).unwrap();
        assert!(t.contains("type=wifi\n") && t.contains("ssid=Home\n"), "{t}");
        assert!(t.contains("key-mgmt=wpa-psk\npsk=correct\\shorse\n"), "{t}");
        assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(sync_network(&s.0, &home(), &[]).unwrap(), Outcome::Unchanged);
    }

    #[test]
    fn open_network_has_no_security_group() {
        let t = render(&Network { ssid: b"Cafe".to_vec(), psk: None });
        assert!(!t.contains("wifi-security"), "{t}");
    }

    #[test]
    fn linux_profile_with_the_same_ssid_wins() {
        let s = Scratch::new("linux");
        fs::write(s.0.join("Home.nmconnection"), "[connection]\nid=Home\n[wifi]\nssid=Home\n").unwrap();
        let linux = linux_ssids(&s.0).unwrap();
        assert_eq!(sync_network(&s.0, &home(), &linux).unwrap(), Outcome::KeptLinuxProfile);
        assert!(!s.0.join(file_name(b"Home")).exists());
    }

    #[test]
    fn new_macos_key_keeps_linux_edits() {
        let s = Scratch::new("refresh");
        sync_network(&s.0, &home(), &[]).unwrap();
        let p = s.0.join(file_name(b"Home"));
        let edited = fs::read_to_string(&p).unwrap().replace("type=wifi\n", "type=wifi\nautoconnect-priority=5\n");
        fs::write(&p, &edited).unwrap();
        let newkey = Network { psk: Some(Secret::Psk([0x22; 32])), ..home() };
        assert_eq!(sync_network(&s.0, &newkey, &[]).unwrap(), Outcome::Updated);
        let t = fs::read_to_string(&p).unwrap();
        assert!(t.contains("autoconnect-priority=5\n") && t.contains(&format!("psk={}\n", "22".repeat(32))), "{t}");
        // Our own files are not mistaken for Linux profiles.
        assert!(linux_ssids(&s.0).unwrap().is_empty());
    }
}
