// SPDX-License-Identifier: MIT
//! Runs the aurora-nvram binary against a synthetic NVRAM v3 image.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

const BANK_SIZE: usize = 0x10000;

/// An erased two-bank v3 store with a valid header in bank 0.
fn empty_v3_image() -> Vec<u8> {
    #[rustfmt::skip]
    const HEADER: [u8; 24] = [
        0x33, 0x56, 0x56, 0x4e, 0x00, 0x00, 0x01, 0x00,
        0x01, 0x00, 0x00, 0x00, 0xfe, 0x5a, 0x01, 0x00,
        0x00, 0x40, 0x00, 0x00, 0x00, 0xc0, 0x00, 0x00,
    ];
    let mut data = vec![0xff; 2 * BANK_SIZE];
    data[..HEADER.len()].copy_from_slice(&HEADER);
    data
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "aurora-nvram-cli-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn image(&self) -> PathBuf {
        let path = self.0.join("nvram.bin");
        fs::write(&path, empty_v3_image()).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(image: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aurora-nvram"))
        .arg("-d")
        .arg(image)
        .args(args)
        .output()
        .unwrap()
}

fn stdout(out: &Output) -> String {
    assert!(
        out.status.success(),
        "aurora-nvram failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout.clone()).unwrap()
}

#[test]
fn write_then_read_back() {
    let s = Scratch::new("rw");
    let image = s.image();
    stdout(&run(
        &image,
        &["write", "system:StartupMute=%01", "common:aurora-test=on"],
    ));
    assert_eq!(
        stdout(&run(&image, &["read", "system:StartupMute"])),
        "system:StartupMute=%01\n"
    );
    let all = stdout(&run(&image, &["read"]));
    assert!(all.contains("system:StartupMute=%01\n"), "{all}");
    assert!(all.contains("common:aurora-test=on\n"), "{all}");
}

#[test]
fn delete_removes_a_variable() {
    let s = Scratch::new("delete");
    let image = s.image();
    stdout(&run(&image, &["write", "common:aurora-test=on"]));
    stdout(&run(&image, &["delete", "common:aurora-test"]));
    assert!(!run(&image, &["read", "common:aurora-test"]).status.success());
}

#[test]
fn reads_do_not_need_write_access() {
    let s = Scratch::new("ro");
    let image = s.image();
    stdout(&run(&image, &["write", "system:StartupMute=%01"]));
    let before = fs::read(&image).unwrap();
    fs::set_permissions(&image, fs::Permissions::from_mode(0o444)).unwrap();
    assert_eq!(
        stdout(&run(&image, &["read", "system:StartupMute"])),
        "system:StartupMute=%01\n"
    );
    assert_eq!(fs::read(&image).unwrap(), before);
}

#[test]
fn missing_variable_fails_without_a_panic() {
    let s = Scratch::new("missing");
    let image = s.image();
    let out = run(&image, &["read", "system:does-not-exist"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("VariableNotFound"), "{err}");
    assert!(!err.contains("panicked"), "{err}");
}

#[test]
fn missing_device_fails_without_a_panic() {
    let out = Command::new(env!("CARGO_BIN_EXE_aurora-nvram"))
        .args(["-d", "/nonexistent/aurora-nvram", "read"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("Open"), "{err}");
    assert!(!err.contains("panicked"), "{err}");
}
