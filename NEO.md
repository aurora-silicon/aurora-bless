# MacBook Neo support

The `neo` branch is the normal read/write `asahi-bless` CLI, based on upstream
0.4.4. It does not introduce read-only/read-write modes or alternate binaries.

MacBook Neo's J700 NVRAM partition contains 29 banks. Upstream commit
`70b16a288cfc6795bc1b145255325f59f000be28` supplies the arbitrary-bank parser
used here. A 29-bank regression test keeps that device ABI covered.

Non-mutating `--list-volumes` and `--get-boot` operations open the NVRAM MTD
read-only. Mutating operations retain upstream's read/write open and normal
erase/program behavior.

The matching T8140 QSPIMC Linux driver and J700 device tree live in the
`nvram-qspi` branch of `aurora-silicon/linux`.

## Qualified build

The `0.4.4-neo.1` aarch64 binary with SHA-256
`44bb88af037aa4d8404be5e9061e67a4da188c193035c933f94c2c0998fbce1f`
was qualified on J700 against writable MTD flags `0xc00`. It listed the macOS
and Omarchy volumes, set the next-only target to the already-selected Omarchy
volume, reparsed that selection, and produced identical repeated full-NVRAM
reads with no new QSPIMC timeout.

## Boot NOR write window

The Aurora `spi-apple-qspi` driver starts with the boot NOR read-only
(`/sys/module/spi_apple_qspi/parameters/read_only` is `Y`). `aurora-bless` and
`aurora-nvram` writes open a scoped window: they lock the NVRAM device, set
`read_only` to `N` only if it was `Y`, apply and verify the change, and set it
back to `Y` on every exit path, including errors. SIGINT, SIGTERM, SIGHUP and
SIGQUIT are held off while the window is open. Erase failures are returned as
errors instead of aborting the process. Where the parameter does not exist the
window only takes the lock.
