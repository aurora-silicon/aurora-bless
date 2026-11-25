// SPDX-License-Identifier: MIT
//! systemd and BlueZ over the system bus.

use std::{
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use dbus::{
    arg::{prop_cast, PropMap},
    blocking::{
        stdintf::org_freedesktop_dbus::{
            ObjectManager, Properties, PropertiesPropertiesChanged,
        },
        Connection,
    },
    message::SignalArgs,
    Message, Path,
};

use crate::parse::Mac;
use crate::sync::{format_mac, Bluez};

const UNIT: &str = "bluetooth.service";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const RESTART_TIMEOUT: Duration = Duration::from_secs(20);
const ADAPTER_TIMEOUT: Duration = Duration::from_secs(60);

/// The last element of a device's object path.
fn dev_node(dev: &Mac) -> String {
    format!("dev_{}", format_mac(dev).replace(':', "_"))
}

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

    /// The object path of the adapter with this address, if BlueZ has it,
    /// and whether it is powered.
    fn adapter(&self, adapter: &Mac) -> Result<Option<(Path<'static>, bool)>, String> {
        let con = self.con()?;
        let bluez = con.with_proxy("org.bluez", "/", Duration::from_secs(5));
        let objects = bluez.get_managed_objects().map_err(|e| e.to_string())?;
        let want = format_mac(adapter);
        Ok(objects.into_iter().find_map(|(path, ifaces)| {
            let props: &PropMap = ifaces.get("org.bluez.Adapter1")?;
            let addr: &String = prop_cast(props, "Address")?;
            let powered = prop_cast::<bool>(props, "Powered").copied().unwrap_or(false);
            addr.eq_ignore_ascii_case(&want).then_some((path, powered))
        }))
    }

    fn adapter_path(&self, adapter: &Mac) -> Result<Option<Path<'static>>, String> {
        Ok(self.adapter(adapter)?.map(|(path, _)| path))
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
        let dev_path = format!("{}/{}", adapter_path, dev_node(dev));
        println!("connecting {dev_path}");
        let device = con.with_proxy("org.bluez", dev_path, CONNECT_TIMEOUT);
        device
            .method_call::<(), _, _, _>("org.bluez.Device1", "Connect", ())
            .map_err(|e| e.to_string())
    }

    fn wait_adapter(&self, adapter: &Mac) {
        let start = Instant::now();
        while start.elapsed() < ADAPTER_TIMEOUT {
            if matches!(self.adapter(adapter), Ok(Some((_, true)))) {
                return;
            }
            thread::sleep(Duration::from_millis(500));
        }
    }

    fn wait_connected(&self, adapter: &Mac, devs: &[Mac]) -> Result<Mac, String> {
        let con = self.con()?;
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let sender = "org.bluez".into();
        let rule = PropertiesPropertiesChanged::match_rule(Some(&sender), None).static_clone();
        let sink = seen.clone();
        let token = con
            .add_match(
                rule,
                move |p: PropertiesPropertiesChanged, _: &Connection, msg: &Message| {
                    let connected = p.interface_name == "org.bluez.Device1"
                        && prop_cast::<bool>(&p.changed_properties, "Connected") == Some(&true);
                    if let (true, Some(path)) = (connected, msg.path()) {
                        sink.lock().unwrap().push(path.to_string());
                    }
                    true
                },
            )
            .map_err(|e| e.to_string())?;
        let found = loop {
            let paths: Vec<String> = seen.lock().unwrap().drain(..).collect();
            if !paths.is_empty() {
                // The adapter's path can change when bluetoothd restarts.
                if let Ok(Some(a)) = self.adapter_path(adapter) {
                    let hit = devs
                        .iter()
                        .find(|d| paths.contains(&format!("{}/{}", a, dev_node(d))));
                    if let Some(dev) = hit {
                        break Ok(*dev);
                    }
                }
            }
            if let Err(e) = con.process(Duration::from_secs(60)) {
                break Err(e.to_string());
            }
        };
        let _ = con.remove_match(token);
        found
    }
}
