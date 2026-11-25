// SPDX-License-Identifier: MIT
//! aurora-btsync: use the Bluetooth pairings macOS made on Linux too.

use std::{
    fs,
    io::{stdout, Write},
    path::Path,
    process::ExitCode,
};

use apple_nvram::{nvram_parse, VarType};

mod dbus;
mod keyfile;
mod parse;
mod sync;

use parse::{parse_ble_info, parse_bt_info};
use sync::format_mac;

const BT_VAR: &str = "BluetoothUHEDevices";
const BLE_VAR: &str = "BluetoothInfo";
const BLUEZ_DIR: &str = "/var/lib/bluetooth";

fn main() -> ExitCode {
    match real_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aurora-btsync: {e}");
            ExitCode::FAILURE
        }
    }
}

fn real_main() -> Result<(), String> {
    let matches = clap::command!()
        .arg(clap::arg!(-d --device [DEVICE] "Path to the nvram device.")
            .default_value("/dev/mtd/by-name/nvram"))
        .subcommand(clap::Command::new("list").about("List the Bluetooth devices paired in macOS"))
        .subcommand(
            clap::Command::new("sync")
                .about("Add or refresh the macOS pairings in BlueZ's storage")
                .arg(clap::arg!(-c --config [CONFIG] "BlueZ storage directory.")
                    .default_value(BLUEZ_DIR))
                .arg(clap::arg!(-s --state [STATE] "Where to record the keys last synced.")
                    .default_value("/var/lib/aurora-btsync/devices")),
        )
        .subcommand(
            clap::Command::new("resolve")
                .about("Have bluetoothd read the services of the BR/EDR devices from macOS")
                .arg(clap::arg!(-c --config [CONFIG] "BlueZ storage directory.")
                    .default_value(BLUEZ_DIR)),
        )
        .subcommand(clap::Command::new("dump").about("Dump the raw BR/EDR device record"))
        .get_matches();

    let device = matches.get_one::<String>("device").unwrap();
    let data = fs::read(device).map_err(|e| format!("{device}: {e}"))?;
    let mut nv = nvram_parse(&data).map_err(|e| format!("{device}: {e:?}"))?;
    let active = nv.active_part_mut();
    let Some(bt_var) = active.get_variable(BT_VAR.as_bytes(), VarType::System) else {
        println!("no Bluetooth pairings in NVRAM");
        return Ok(());
    };
    let bt_raw = bt_var.value();
    let ble_raw = active
        .get_variable(BLE_VAR.as_bytes(), VarType::System)
        .map(|v| v.value());

    if let Some(("dump", _)) = matches.subcommand() {
        return stdout().write_all(&bt_raw).map_err(|e| e.to_string());
    }

    let bt = parse_bt_info(&bt_raw).map_err(|e| format!("{BT_VAR}: {e}"))?;
    let ble = match &ble_raw {
        Some(raw) => parse_ble_info(raw).map_err(|e| format!("{BLE_VAR}: {e}"))?,
        None => Vec::new(),
    };

    match matches.subcommand() {
        Some(("sync", args)) => {
            let config = Path::new(args.get_one::<String>("config").unwrap());
            if !config.is_dir() {
                return Err(format!("{}: BlueZ storage directory not found", config.display()));
            }
            let state_path = Path::new(args.get_one::<String>("state").unwrap());
            let mut state = sync::State::load(state_path)
                .map_err(|e| format!("{}: {e}", state_path.display()))?;
            let opts = sync::Options {
                notify_bluetoothd: config == Path::new(BLUEZ_DIR),
                restart_after_exit: std::env::var_os("AURORA_BTSYNC_UNIT").is_some(),
            };
            sync::sync(config, &bt, &ble, &mut state, &dbus::SystemBus::new(), &opts)?;
            Ok(())
        }
        Some(("resolve", args)) => {
            let config = Path::new(args.get_one::<String>("config").unwrap());
            sync::resolve(config, &bt, &dbus::SystemBus::new())
        }
        _ => {
            for d in &bt.devices {
                println!(
                    "ID {:04x}:{:04x} {} ({}) BR/EDR",
                    d.vendor_id,
                    d.product_id,
                    d.name,
                    format_mac(&d.mac)
                );
            }
            for d in &ble {
                println!(
                    "ID {:04x}:{:04x} {} ({}) LE",
                    d.vendor_id,
                    d.product_id,
                    d.name,
                    format_mac(&d.mac)
                );
            }
            Ok(())
        }
    }
}
