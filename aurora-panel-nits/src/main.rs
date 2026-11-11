// SPDX-License-Identifier: MIT
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    os::fd::AsFd,
    path::PathBuf,
    process::ExitCode,
    time::{Duration, Instant},
};

use aurora_panel_nits::{
    clamp_restore, read_nits, savable, store_nits, Backlight, Limiter, BACKLIGHT, DEVICE,
};
use nix::{
    poll::{poll, PollFd, PollFlags, PollTimeout},
    sys::{
        signal::{SigSet, Signal},
        signalfd::{SfdFlags, SignalFd},
    },
};

const USAGE: &str = "usage: aurora-panel-nits [--device PATH] [--backlight NAME] restore|save|watch

  restore  start the panel at the NVRAM backlight-nits level (the boot loader's)
  save     store the panel's level in NVRAM now
  watch    restore once per boot, then store the user's level, rate limited,
           and once more on exit

Environment (watch): AURORA_PANEL_NITS_DEBOUNCE_SEC (default 5),
AURORA_PANEL_NITS_MIN_INTERVAL_SEC (default 60).";

/// Marks that this boot's panel level came from NVRAM.
const RESTORED: &str = "/run/aurora-panel-nits.restored";

/// Re-read the level at least this often, in case a change sent no event.
const RECHECK: Duration = Duration::from_secs(300);

struct Args {
    device: PathBuf,
    backlight: String,
    command: String,
}

fn parse_args() -> Option<Args> {
    let mut device = PathBuf::from(DEVICE);
    let mut backlight = BACKLIGHT.to_owned();
    let mut command = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-d" | "--device" => device = args.next()?.into(),
            "-b" | "--backlight" => backlight = args.next()?,
            "restore" | "save" | "watch" if command.is_none() => command = Some(a),
            _ => return None,
        }
    }
    Some(Args {
        device,
        backlight,
        command: command?,
    })
}

fn env_secs(name: &str, default: u64) -> Duration {
    Duration::from_secs(
        std::env::var(name)
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(default),
    )
}

fn main() -> ExitCode {
    let Some(args) = parse_args() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let bl = Backlight::new(&args.backlight);
    if !bl.exists() {
        eprintln!("no backlight device {}", args.backlight);
        return ExitCode::SUCCESS;
    }
    let ok = match args.command.as_str() {
        "restore" => restore(&args, &bl),
        "save" => save(&args, &bl),
        _ => watch(&args, &bl),
    };
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn restore(args: &Args, bl: &Backlight) -> bool {
    match read_nits(&args.device) {
        Ok(stored) => restore_once(args, bl, stored),
        Err(e) => {
            // The NVRAM driver can load after the display: the service that
            // starts with the NVRAM device sets the level then.
            println!(
                "NVRAM {} not readable yet ({e}): the panel keeps the boot loader's level",
                args.device.display()
            );
            true
        }
    }
}

/// Starts the panel at the stored level, once per boot. Until then nothing
/// touches the level, so the panel keeps the boot loader's.
fn restore_once(args: &Args, bl: &Backlight, stored: Option<u32>) -> bool {
    if std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(RESTORED)
        .is_err()
    {
        return true;
    }
    let (max, current) = match (bl.max(), bl.level()) {
        (Ok(m), Ok(c)) => (m, c),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("{}: {e}", args.backlight);
            return false;
        }
    };
    let Some(nits) = stored else {
        println!("no backlight-nits in NVRAM: keeping {current} of {max}");
        return true;
    };
    let level = clamp_restore(nits, max);
    println!("starting at the boot loader's level: {nits} nits (set {level} of {max})");
    // Written even when unchanged: a panel that presents nothing until its
    // first level (the J700) shows the loader's level from here on.
    match bl.set_level(level) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("{}: {e}", args.backlight);
            false
        }
    }
}

fn store(args: &Args, level: u32) -> bool {
    match store_nits(&args.device, level) {
        Ok(true) => {
            println!("stored {level} nits in NVRAM backlight-nits");
            true
        }
        Ok(false) => true,
        Err(e) => {
            eprintln!("storing {level} nits: {e}");
            false
        }
    }
}

fn current(bl: &Backlight) -> Option<u32> {
    savable(bl.level().ok()?, bl.power())
}

fn save(args: &Args, bl: &Backlight) -> bool {
    match current(bl) {
        Some(level) => store(args, level),
        None => true,
    }
}

fn watch(args: &Args, bl: &Backlight) -> bool {
    let debounce = env_secs("AURORA_PANEL_NITS_DEBOUNCE_SEC", 5);
    let interval = env_secs("AURORA_PANEL_NITS_MIN_INTERVAL_SEC", 60);

    let mut mask = SigSet::empty();
    for s in [Signal::SIGTERM, Signal::SIGINT, Signal::SIGHUP] {
        mask.add(s);
    }
    if let Err(e) = mask.thread_block() {
        eprintln!("signal mask: {e}");
        return false;
    }
    let signals = match SignalFd::with_flags(&mask, SfdFlags::SFD_CLOEXEC) {
        Ok(fd) => fd,
        Err(e) => {
            eprintln!("signalfd: {e}");
            return false;
        }
    };
    // The backlight core notifies actual_brightness on every level change.
    let mut notify = match File::open(bl.path("actual_brightness")) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{}: {e}", args.backlight);
            return false;
        }
    };
    let mut scratch = [0u8; 32];
    let _ = notify.read(&mut scratch);

    let stored = match read_nits(&args.device) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("NVRAM {} not readable: {e}", args.device.display());
            return false;
        }
    };
    // The panel starts at the stored level before anything is stored, so a
    // level picked before NVRAM was readable never replaces the user's.
    restore_once(args, bl, stored);
    let mut limiter = Limiter::new(debounce, interval, stored);
    println!(
        "watching {} (NVRAM level {}, debounce {}s, interval {}s)",
        args.backlight,
        stored.map_or("none".to_owned(), |n| format!("{n} nits")),
        debounce.as_secs(),
        interval.as_secs()
    );
    if let Some(level) = current(bl) {
        limiter.observe(level, Instant::now());
    }

    loop {
        let now = Instant::now();
        if let Some(level) = limiter.due(now) {
            let ok = store(args, level);
            limiter.wrote(level, now, ok);
        }
        let wait = limiter
            .deadline()
            .map_or(RECHECK, |d| d.saturating_duration_since(Instant::now()).min(RECHECK));
        let timeout = PollTimeout::try_from(wait.as_millis().max(1) as i32).unwrap_or(PollTimeout::MAX);
        let mut fds = [
            PollFd::new(signals.as_fd(), PollFlags::POLLIN),
            PollFd::new(notify.as_fd(), PollFlags::POLLPRI | PollFlags::POLLERR),
        ];
        match poll(&mut fds, timeout) {
            Ok(_) => {}
            Err(nix::errno::Errno::EINTR) => continue,
            Err(e) => {
                eprintln!("poll: {e}");
                return false;
            }
        }
        let stop = fds[0].revents().is_some_and(|r| !r.is_empty());
        if fds[1].revents().is_some_and(|r| !r.is_empty()) {
            // Re-arm the notification.
            if notify.seek(SeekFrom::Start(0)).is_err() || notify.read(&mut scratch).is_err() {
                // The device went away with its display.
                break;
            }
        }
        match current(bl) {
            Some(level) => limiter.observe(level, Instant::now()),
            None if !bl.exists() => break,
            None => {}
        }
        if stop {
            break;
        }
    }
    // Stopping (shutdown, logout of the display): store what is pending now.
    match limiter.flush() {
        Some(level) => store(args, level),
        None => true,
    }
}
