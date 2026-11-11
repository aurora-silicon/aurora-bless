// SPDX-License-Identifier: MIT
//! systemd and BlueZ over the system bus.

use std::{
    thread,
    time::{Duration, Instant},
};

use dbus::{
    arg::{prop_cast, PropMap},
    blocking::{
        stdintf::org_freedesktop_dbus::{ObjectManager, Properties},
        Connection,
    },
    Path,
};

use crate::parse::Mac;
use crate::sync::{format_mac, Bluez};

const UNIT: &str = "bluetooth.service";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const RESTART_TIMEOUT: Duration = Duration::from_secs(20);

pub struct SystemBus {
    con: Option<Connection>,
}

impl SystemBus {
    pub fn new() -> SystemBus {
        SystemBus {
            con: Connection::new_system().ok(),
        }
    }

    fn con(&self) -> Result<&Connection, String> {
        self.con
            .as_ref()
            .ok_or_else(|| "no system bus connection".to_string())
    }

    fn unit_state(&self) -> Result<String, String> {
        let con = self.con()?;
        let systemd = con.with_proxy(
            "org.freedesktop.systemd1",
            "/org/freedesktop/systemd1",
            Duration::from_secs(5),
        );
        let (unit,): (Path,) = systemd
            .method_call("org.freedesktop.systemd1.Manager", "LoadUnit", (UNIT,))
            .map_err(|e| e.to_string())?;
        let unit = con.with_proxy("org.freedesktop.systemd1", unit, Duration::from_secs(5));
        unit.get("org.freedesktop.systemd1.Unit", "ActiveState")
            .map_err(|e| e.to_string())
    }

    /// The object path of the adapter with this address, if BlueZ has it.
    fn adapter_path(&self, adapter: &Mac) -> Result<Option<Path<'static>>, String> {
        let con = self.con()?;
        let bluez = con.with_proxy("org.bluez", "/", Duration::from_secs(5));
        let objects = bluez.get_managed_objects().map_err(|e| e.to_string())?;
        let want = format_mac(adapter);
        Ok(objects.into_iter().find_map(|(path, ifaces)| {
            let props: &PropMap = ifaces.get("org.bluez.Adapter1")?;
            let addr: &String = prop_cast(props, "Address")?;
            addr.eq_ignore_ascii_case(&want).then_some(path)
        }))
    }
}

impl Bluez for SystemBus {
    fn running(&self) -> bool {
        matches!(
            self.unit_state().as_deref(),
            Ok("active") | Ok("activating") | Ok("reloading")
        )
    }

    fn restart(&self, adapter: &Mac, wait: bool) -> Result<(), String> {
        let con = self.con()?;
        let systemd = con.with_proxy(
            "org.freedesktop.systemd1",
            "/org/freedesktop/systemd1",
            Duration::from_secs(5),
        );
        // bluetoothd only reads the storage directory when it starts.
        let _: (Path,) = systemd
            .method_call(
                "org.freedesktop.systemd1.Manager",
                "TryRestartUnit",
                (UNIT, "replace"),
            )
            .map_err(|e| format!("restarting {UNIT}: {e}"))?;
        if !wait {
            return Ok(());
        }
        let start = Instant::now();
        while start.elapsed() < RESTART_TIMEOUT {
            thread::sleep(Duration::from_millis(250));
            if self.unit_state().as_deref() == Ok("active")
                && matches!(self.adapter_path(adapter), Ok(Some(_)))
            {
                return Ok(());
            }
        }
        Err(format!(
            "adapter {} did not come back within {}s",
            format_mac(adapter),
            RESTART_TIMEOUT.as_secs()
        ))
    }

    fn connect(&self, adapter: &Mac, dev: &Mac) -> Result<(), String> {
        let con = self.con()?;
        let adapter_path = self
            .adapter_path(adapter)?
            .ok_or_else(|| format!("adapter {} not found", format_mac(adapter)))?;
        let dev_path = format!("{}/dev_{}", adapter_path, format_mac(dev).replace(':', "_"));
        println!("connecting {dev_path}");
        let device = con.with_proxy("org.bluez", dev_path, CONNECT_TIMEOUT);
        device
            .method_call::<(), _, _, _>("org.bluez.Device1", "Connect", ())
            .map_err(|e| e.to_string())
    }
}
