// SPDX-License-Identifier: MIT
//! Use the Wi-Fi networks saved in macOS on Linux: macOS shares them with
//! its boot environment in the `preferred-networks` NVRAM variable, and
//! `aurora-wifisync sync` turns them into NetworkManager profiles. Keys are
//! never printed or logged.

mod nm;
mod parse;

use std::{fs, os::unix::fs::DirBuilderExt, path::Path, process::ExitCode, time::Duration};

use apple_nvram::{nvram_parse, VarType};

const WLAN_VAR: &str = "preferred-networks";
const NM_DIR: &str = "/etc/NetworkManager/system-connections";

fn main() -> ExitCode {
    match real_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aurora-wifisync: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Asks a running NetworkManager to load the new files. NM that is not
/// running reads them when it starts.
fn reload_networkmanager() {
    let Ok(con) = dbus::blocking::Connection::new_system() else { return };
    let proxy = con.with_proxy(
        "org.freedesktop.NetworkManager",
        "/org/freedesktop/NetworkManager/Settings",
        Duration::from_secs(10),
    );
    let r: Result<(bool,), _> =
        proxy.method_call("org.freedesktop.NetworkManager.Settings", "ReloadConnections", ());
    if r.is_ok() {
        println!("NetworkManager reloaded its profiles");
    }
}

fn real_main() -> Result<(), String> {
    let matches = clap::command!()
        .arg(clap::arg!(-d --device [DEVICE] "Path to the nvram device.")
            .default_value("/dev/mtd/by-name/nvram"))
        .subcommand(clap::Command::new("list").about("List the Wi-Fi networks saved in macOS (no keys printed)"))
        .subcommand(
            clap::Command::new("sync")
                .about("Add the Wi-Fi networks saved in macOS to NetworkManager")
                .arg(clap::arg!(-c --config [CONFIG] "NetworkManager system-connections directory.")
                    .default_value(NM_DIR)),
        )
        .get_matches();

    let device = matches.get_one::<String>("device").unwrap();
    let data = fs::read(device).map_err(|e| format!("{device}: {e}"))?;
    let mut nv = nvram_parse(&data).map_err(|e| format!("{device}: {e:?}"))?;
    let active = nv.active_part_mut();
    let Some(var) = active.get_variable(WLAN_VAR.as_bytes(), VarType::System) else {
        println!("no Wi-Fi networks saved in NVRAM");
        return Ok(());
    };
    let nets = parse::parse_networks(&var.value());

    match matches.subcommand() {
        Some(("sync", args)) => {
            let dir = Path::new(args.get_one::<String>("config").unwrap());
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .map_err(|e| format!("{}: {e}", dir.display()))?;
            let linux = nm::linux_ssids(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            let mut changed = false;
            for net in &nets {
                match nm::sync_network(dir, net, &linux) {
                    Ok(nm::Outcome::Added) => {
                        println!("{}: added", net.ssid_lossy());
                        changed = true;
                    }
                    Ok(nm::Outcome::Updated) => {
                        println!("{}: new key from macOS", net.ssid_lossy());
                        changed = true;
                    }
                    Ok(nm::Outcome::KeptLinuxProfile) => {
                        println!("{}: kept the profile made on Linux", net.ssid_lossy())
                    }
                    Ok(nm::Outcome::Unchanged) => {}
                    Err(e) => eprintln!("{}: {e}", net.ssid_lossy()),
                }
            }
            if changed && dir == Path::new(NM_DIR) {
                reload_networkmanager();
            }
            Ok(())
        }
        _ => {
            for net in &nets {
                let sec = if net.psk.is_some() { "WPA-PSK" } else { "open" };
                println!("SSID {} ({sec})", net.ssid_lossy());
            }
            Ok(())
        }
    }
}
