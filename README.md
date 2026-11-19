# aurora-bless

NVRAM tools for Apple silicon Macs, based on the Asahi Linux asahi-nvram
workspace (MIT; the Asahi Linux copyright notices are kept in each LICENSE).

| Crate | Installed as |
| --- | --- |
| `aurora-bless` | `aurora-bless`: pick the startup volume |
| `aurora-nvram` | `aurora-nvram`: read, write and delete NVRAM variables |
| `aurora-btsync` | `aurora-btsync`: use the Bluetooth pairings made in macOS |
| `aurora-panel-nits` | `aurora-panel-nits`: share the panel brightness with the boot loader |
| `apple-nvram` | library used by all of the above |
| `aurora-wifisync` | not packaged |

Distribution packages may install the pre-rename command names as symlinks
for scripts that still call them, until 2027-02-28.

`apple-nvram` here parses the NVRAM v3 stores of current Macs (any number of
banks, records bounded by the declared store size), and writes go through a
scoped boot NOR write window. See [NEO.md](NEO.md) for the MacBook Neo
support and the write window.

Writing NVRAM can leave a Mac that needs a DFU restore if something goes
wrong. Back up the device (`dd if=/dev/mtd/by-name/nvram of=nvram.bin`)
before writing, and try a write on a copy first with `-d nvram-copy.bin`.
