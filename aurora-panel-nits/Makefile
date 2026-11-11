PREFIX ?= /usr
BINDIR ?= $(PREFIX)/bin
UNITDIR ?= $(PREFIX)/lib/systemd/system
UDEVDIR ?= $(PREFIX)/lib/udev/rules.d
DESTDIR ?=
BIN ?= ../target/release/aurora-panel-nits

install:
	install -Dm755 $(BIN) $(DESTDIR)$(BINDIR)/aurora-panel-nits
	install -Dm644 systemd/aurora-panel-nits.service $(DESTDIR)$(UNITDIR)/aurora-panel-nits.service
	install -Dm644 systemd/50-aurora-panel-nits.conf \
		'$(DESTDIR)$(UNITDIR)/systemd-backlight@backlight:apple-panel-bl.service.d/50-aurora-panel-nits.conf'
	install -Dm644 systemd/90-aurora-panel-nits.rules $(DESTDIR)$(UDEVDIR)/90-aurora-panel-nits.rules

.PHONY: install
