# aurora-btsync

Lets Linux use Bluetooth devices that were paired in macOS, without pairing
them again. macOS keeps its pairings in NVRAM (`BluetoothUHEDevices` for
BR/EDR devices, `BluetoothInfo` for LE devices); `aurora-btsync sync` writes
them into BlueZ's storage under `/var/lib/bluetooth`. It replaces
`asahi-btsync`, which stays available as a symlink, and
`asahi-btsync.service` is an alias of `aurora-btsync.service`.

    aurora-btsync list     # the devices paired in macOS (no keys printed)
    aurora-btsync sync     # needs root
    aurora-btsync resolve  # needs root and a running bluetoothd

The udev rule starts `aurora-btsync.service` when the NVRAM device appears.
The unit runs before `bluetooth.service`, so at boot bluetoothd reads the
keys when it starts and needs no restart. It also starts
`aurora-btsync-resolve.service`, which runs `aurora-btsync resolve` once
bluetoothd is up.

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
- A BR/EDR device of major class Peripheral (keyboard, mouse, trackpad,
  game controller) gets the HID service in `Services` if its file has no
  `Services`, including files written by older versions. bluetoothd only
  registers a device as an input device when it lists that service. It
  answers a HID connection from a device it has not registered with a
  virtual cable unplug, and the device then deletes its pairing for this
  controller address, which is the one macOS uses too.
- When bluetoothd is already running and the storage directory is its own
  (`/var/lib/bluetooth`, not one given with `-c`), it is restarted so it loads the new
  keys, and only the devices that were added or updated are connected. A
  device that is off or out of range times out; that is logged and the sync
  still succeeds.

Files are written atomically with mode 0600.

## What resolve does

NVRAM holds a BR/EDR device's link key but not its SDP records, and BlueZ
needs them (for an input device, the HID record carries the report
descriptor). bluetoothd only reads them when it pairs a device itself or
is asked to connect one. `resolve` lists the BR/EDR devices from NVRAM
that BlueZ has a pairing for but no records (no `[ServiceRecords]` in
`/var/lib/bluetooth/<adapter>/cache/<device>`), waits for each to connect
(a key press wakes a keyboard, which then connects to the last host it
used), and then calls `Device1.Connect` on it: bluetoothd reads and stores
the records over the device's own link, then connects its profiles.

It never pages a device that is not connected. When bluetoothd fails to
read a device's records (for example because the device is off), it still
marks the device's services as resolved and does not try again until it
restarts; every later connect then skips the records and fails. So if a
read fails anyway, `resolve` restarts bluetoothd (at most twice) and waits
for the device to connect again. It exits when every device has its
records, so after the first time it exits at once.
