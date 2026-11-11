# aurora-nvram

Reads, writes and deletes NVRAM variables on Apple silicon Macs. It replaces
`asahi-nvram`, which is installed as a symlink to it so existing scripts keep
working.

    aurora-nvram read                          # every variable
    aurora-nvram read system:StartupMute       # one variable
    aurora-nvram write system:StartupMute=%01  # needs root
    aurora-nvram delete common:some-variable
    aurora-nvram -d nvram-copy.bin read        # a dump instead of the device

Variables are `partition:name`, with partition `system` or `common`. Values
are printed and written with bytes that are not printable ASCII as `%xx`.

`read` opens the device read-only, so it works while the boot NOR is
read-only and cannot change it. `write` and `delete` take the NVRAM lock and
open the boot NOR write window for the duration of the change (see
[NEO.md](../NEO.md)). With `-d` pointing at a regular file, nothing outside
that file is touched.

Changes from asahi-nvram 0.2.5: the shared `apple-nvram` parses the NVRAM v3
stores of current Macs instead of panicking on them, writes use the write
window, reads no longer need write access, and errors are reported instead
of panicking.
