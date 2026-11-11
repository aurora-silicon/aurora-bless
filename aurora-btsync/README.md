# aurora-btsync

Lets Linux use Bluetooth devices that were paired in macOS, without pairing
them again. macOS keeps its pairings in NVRAM (`BluetoothUHEDevices` for
BR/EDR devices, `BluetoothInfo` for LE devices); `aurora-btsync sync` writes
them into BlueZ's storage under `/var/lib/bluetooth`. It replaces
`asahi-btsync`, which stays available as a symlink, and
`asahi-btsync.service` is an alias of `aurora-btsync.service`.

    aurora-btsync list   # the devices paired in macOS (no keys printed)
    aurora-btsync sync   # needs root

The udev rule starts `aurora-btsync.service` when the NVRAM device appears.
The unit runs before `bluetooth.service`, so at boot bluetoothd reads the
keys when it starts and needs no restart.

## What sync does

- A device in NVRAM that BlueZ does not know gets a new `info` file.
- A device BlueZ already knows gets its key groups replaced (`LinkKey` for
  BR/EDR; `IdentityResolvingKey`, `LongTermKey`, `PeripheralLongTermKey` and
  `SlaveLongTermKey` for LE). Everything else in the file (services, name,
  appearance, connection parameters, the link key type) is kept.
- The keys are only replaced when macOS changed them since the last sync.
  `/var/lib/aurora-btsync/devices` records a fingerprint of the NVRAM keys
  last applied, so a device paired again on Linux afterwards keeps its Linux
  keys until macOS pairs it again.
- Devices that are only paired on Linux are never touched.
- LE devices get `[LongTermKey]`, the key BlueZ uses when Linux is the
  central (the normal role with a mouse, keyboard or trackpad). With legacy
  pairing that is the key the device handed out, with its EDIV and Rand
  (NVRAM tags 0x09, 0x0a, 0x0b). With LE Secure Connections it is the single
  key (tag 0x10), which is also written as `PeripheralLongTermKey`.
- Tag 0x11 is the device's appearance and goes into `[General]` of a new
  file.
- When bluetoothd is already running and the storage directory is its own
  (`/var/lib/bluetooth`, not one given with `-c`), it is restarted so it loads the new
  keys, and only the devices that were added or updated are connected. A
  device that is off or out of range times out; that is logged and the sync
  still succeeds.

Files are written atomically with mode 0600.

## Changes from asahi-btsync 0.2.6

The fixes for existing pairings, `[LongTermKey]`, the appearance tag and
connect timeouts address the problems reported and diagnosed by Ashwin
Sawant (asahi-nvram issue 48, September 2026), including his suggested fix
of writing the peripheral key as `[LongTermKey]`.

- Existing BlueZ pairings are refreshed when macOS re-pairs a device
  instead of being skipped.
- LE devices get `[LongTermKey]`, and legacy-pairing LTKs with their EDIV
  and Rand are used; before, devices paired with legacy pairing got no LTK.
- Tag 0x11 (appearance) is parsed instead of warned about.
- A failed or timed-out `Device1.Connect` no longer fails the sync; only
  added or updated devices are connected.
- bluetoothd is restarted through systemd instead of being sent SIGUSR1,
  which bluetoothd does not handle (it terminated it).
- An empty `BluetoothInfo` no longer creates a device `00:00:00:00:00:00`.
- Malformed records are reported as errors instead of panicking.
- The BR/EDR `[DeviceID]` group name is spelled correctly.
