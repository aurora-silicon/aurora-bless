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
            ObjectManager, ObjectManagerInterfacesAdded, Properties, PropertiesPropertiesChanged,
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

/// The BR/EDR bearer of a device (BlueZ 5.84 and later).
const BREDR_BEARER: &str = "org.bluez.Bearer.BREDR1";

/// Whether the device's BR/EDR link is up. SDP runs over BR/EDR, and
/// `Device1.Connect` on a dual-mode device whose LE link is up pages it over
/// BR/EDR, so an LE connection must not count. BlueZ before the bearer
/// interfaces only has `Device1.Connected`.
fn bredr_connected(con: &Connection, path: &str) -> bool {
    let proxy = con.with_proxy("org.bluez", path, Duration::from_secs(5));
    match proxy.get::<bool>(BREDR_BEARER, "Connected") {
        Ok(c) => c,
        Err(e) if e.name() == Some("org.freedesktop.DBus.Error.InvalidArgs")
            || e.name() == Some("org.freedesktop.DBus.Error.UnknownInterface") =>
        {
            matches!(proxy.get::<bool>("org.bluez.Device1", "Connected"), Ok(true))
        }
        Err(_) => false,
    }
}

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
        let Ok(con) = self.con() else { return };
        // Set when an adapter appears or one of its properties changes.
        let changed: Arc<Mutex<bool>> = Arc::default();
        let sender = "org.bluez".into();
        let rule = PropertiesPropertiesChanged::match_rule(Some(&sender), None).static_clone();
        let sink = changed.clone();
        let props = con.add_match(
            rule,
            move |p: PropertiesPropertiesChanged, _: &Connection, _: &Message| {
                if p.interface_name == "org.bluez.Adapter1" {
                    *sink.lock().unwrap() = true;
                }
                true
            },
        );
        let rule = ObjectManagerInterfacesAdded::match_rule(Some(&sender), None).static_clone();
        let sink = changed.clone();
        let added = con.add_match(
            rule,
            move |p: ObjectManagerInterfacesAdded, _: &Connection, _: &Message| {
                if p.interfaces.contains_key("org.bluez.Adapter1") {
                    *sink.lock().unwrap() = true;
                }
                true
            },
        );
        // Checked after the matches are registered, so an adapter powered on
        // in between is not missed.
        let deadline = Instant::now() + ADAPTER_TIMEOUT;
        let mut check = true;
        loop {
            if check && matches!(self.adapter(adapter), Ok(Some((_, true)))) {
                break;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || con.process(left).is_err() {
                break;
            }
            check = std::mem::take(&mut *changed.lock().unwrap());
        }
        for token in [props, added].into_iter().flatten() {
            let _ = con.remove_match(token);
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
                    // Device1.Connected is true for either bearer; which one
                    // is checked below.
                    let connected = (p.interface_name == "org.bluez.Device1"
                        || p.interface_name == BREDR_BEARER)
                        && prop_cast::<bool>(&p.changed_properties, "Connected") == Some(&true);
                    if let (true, Some(path)) = (connected, msg.path()) {
                        sink.lock().unwrap().push(path.to_string());
                    }
                    true
                },
            )
            .map_err(|e| e.to_string())?;
        // One that is already connected (registered after the match, so a
        // connection in between is not missed).
        if let Ok(Some(a)) = self.adapter_path(adapter) {
            for dev in devs {
                if bredr_connected(con, &format!("{}/{}", a, dev_node(dev))) {
                    let _ = con.remove_match(token);
                    return Ok(*dev);
                }
            }
        }
        let found = loop {
            let paths: Vec<String> = seen.lock().unwrap().drain(..).collect();
            if !paths.is_empty() {
                // The adapter's path can change when bluetoothd restarts.
                if let Ok(Some(a)) = self.adapter_path(adapter) {
                    let hit = devs.iter().find(|d| {
                        let path = format!("{}/{}", a, dev_node(d));
                        paths.contains(&path) && bredr_connected(con, &path)
                    });
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
