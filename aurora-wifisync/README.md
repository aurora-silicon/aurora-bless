# aurora-wifisync

Lets Linux join the Wi-Fi networks saved in macOS on first boot, without
typing the passwords again. macOS shares the networks it knows with its boot
environment in the `preferred-networks` NVRAM variable (SSID and WPA
passphrase); `aurora-wifisync sync` writes them as NetworkManager profiles.

    aurora-wifisync list   # the networks saved in macOS (no keys printed)
    aurora-wifisync sync   # needs root

The udev rule starts `aurora-wifisync.service` when the NVRAM device appears.
The unit runs before `NetworkManager.service`, so at boot NetworkManager
finds the profiles when it starts; when it is already running, the sync asks
it to reload.

## What sync does

- Each network gets `/etc/NetworkManager/system-connections/aurora-macos-<SSID
  in hex>.nmconnection` (root, mode 0600), a WPA-PSK profile with the
  passphrase macOS stored, or an open profile.
- A network that already has a profile made on Linux (any other keyfile with
  the same SSID) is left alone.
- A profile written before only gets its key replaced when macOS has a new
  one; settings changed on Linux are kept. Networks removed in macOS are not
  removed on Linux.
- Keys are never printed or logged.

## The NVRAM records

One 0xc0-byte record per network: security flags at 0x08 (0 = open), SSID
length at 0x0c and SSID at 0x10, the passphrase NUL-padded at 0x58 with its
length at 0x9c. That is the layout current macOS writes (seen on M1 Pro, M4 Pro and
MacBook Neo). A secured record with no
passphrase but a 256-bit key at 0xa0 (where the upstream asahi-wifisync
read it) gets that key instead. Records with an empty or oversized SSID, or
a key NetworkManager cannot use, are skipped. Profiles use `wpa-psk`, which
also joins WPA2/WPA3 transition networks; a WPA3-only network needs its
profile switched to `sae` on Linux.
