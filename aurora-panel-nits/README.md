# aurora-panel-nits

One brightness level for the built-in panel, shared by the boot loader,
Linux and macOS.

The boot loader lights the panel at the level in the NVRAM variable
`system:backlight-nits` (ASCII hex, 16.16 fixed-point nits: `0x008c0000` is
140 nits). The `apple-panel-bl` backlight device counts in nits too.

- `watch` (`aurora-panel-nits.service`, started once both the
  `apple-panel-bl` backlight and the NVRAM device exist) first starts the
  panel at the stored level, once per boot, so it does not step when Linux
  takes over. Until then nothing sets the level, so the panel keeps the
  boot loader's. systemd-backlight's unit for this device runs `restore`
  instead of its own load (which does the same when NVRAM is already
  readable) and saves nothing. Restored levels are kept at or above 5% of
  the maximum; without the variable the kernel's level is kept. It then stores the level the user
  picks. A level is written once it has been stable for 5 s, no sooner than
  60 s after the previous write, and once more when the service stops
  (shutdown). Blanked or zero levels are not stored.
  `AURORA_PANEL_NITS_DEBOUNCE_SEC` and `AURORA_PANEL_NITS_MIN_INTERVAL_SEC`
  in `/etc/default/aurora-panel-nits` change the two times.
- `restore` sets the stored level, once per boot.
- `save` stores the current level once.

Writes take the same lock and boot NOR write window as `aurora-bless` and
`aurora-nvram` (see [NEO.md](../NEO.md)), and skip the write when NVRAM already
holds the level. NVRAM v3 appends each change as a record and erases a 64 KiB
bank only when the store moves to the next one.

To go back to systemd-backlight's own restore, mask the service and add an
empty override for the drop-in:

    systemctl mask aurora-panel-nits.service
    mkdir -p '/etc/systemd/system/systemd-backlight@backlight:apple-panel-bl.service.d'
    ln -s /dev/null '/etc/systemd/system/systemd-backlight@backlight:apple-panel-bl.service.d/50-aurora-panel-nits.conf'

GNOME Shell 51 also saves a level per monitor and sets it when the session
starts. For a built-in panel that level goes stale when the level changes
elsewhere (macOS, the boot loader, another user's session); a shell that
starts built-in panels from the system level keeps the shared one.
