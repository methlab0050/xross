#![allow(irrefutable_let_patterns)]

mod anim;
mod backend;
mod capture;
mod config;
mod cursor;
mod focus;
mod grabs;
mod handlers;
mod input;
mod ipc;
mod layout;
mod render;
mod screenshot;
mod shaders;
mod shell;
mod state;
mod ui;

use input::binds::Profile;
use smithay::reexports::{calloop::EventLoop, wayland_server::Display};
use state::XrossState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    Winit,
    Udev,
}

struct Args {
    backend: Backend,
    binds: Option<Profile>,
    command: Option<String>,
    scale: Option<f64>,
    config: Option<std::path::PathBuf>,
    log: Option<std::path::PathBuf>,
    /// Started as a login session (from a display manager).
    session: bool,
}

const USAGE: &str = "usage: xross [--session] [--backend winit|udev] [--binds dev|prod] [--scale FACTOR] [--config PATH] [--log PATH] [-c|--command CMD]\n       xross msg ACTION...   (run an action in the running xross, e.g. `xross msg workspace right`)";

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        backend: Backend::Winit,
        binds: None,
        command: None,
        scale: None,
        config: None,
        log: None,
        session: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value\n{USAGE}"));
        match flag.as_str() {
            "--backend" => {
                args.backend = match value()?.as_str() {
                    "winit" => Backend::Winit,
                    "udev" => Backend::Udev,
                    other => return Err(format!("unknown backend '{other}'\n{USAGE}")),
                }
            }
            // A login session (the display manager's session entry): DRM/KMS, and the
            // session's environment is shared with systemd and D-Bus activated services.
            "--session" => {
                args.session = true;
                args.backend = Backend::Udev;
            }
            "--binds" => args.binds = Some(value()?.parse()?),
            "-c" | "--command" => args.command = Some(value()?),
            "--config" => args.config = Some(value()?.into()),
            "--log" => args.log = Some(value()?.into()),
            "--scale" => {
                let v = value()?;
                args.scale = Some(v.parse().map_err(|_| format!("invalid scale '{v}'"))?);
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument '{other}'\n{USAGE}")),
        }
    }
    Ok(args)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Client mode: `xross msg snap left`.
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) == Some("msg") {
        if let Err(err) = ipc::send(&argv[1..].join(" ")) {
            eprintln!("xross msg: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }

    let args = parse_args()?;
    let log_path = init_logging(args.log.clone());

    // Nested runs inside GNOME, which grabs Super, so it defaults to the Alt-based dev
    // binds. The config can choose, and --binds overrides both.
    let profile = args.binds.unwrap_or(match args.backend {
        Backend::Winit => Profile::Dev,
        Backend::Udev => Profile::Prod,
    });

    let mut event_loop: EventLoop<XrossState> = EventLoop::try_new()?;
    let display: Display<XrossState> = Display::new()?;
    let mut state = XrossState::new(&mut event_loop, display, profile);
    // Before the backend, so outputs come up with the configured scale.
    state.binds_override = args.binds;
    state.scale_override = args.scale;
    state.init_config(args.config.or_else(config::default_path));

    match args.backend {
        Backend::Winit => backend::winit::init_winit(&mut event_loop, &mut state)?,
        Backend::Udev => backend::udev::init_udev(&mut event_loop, &mut state)?,
    }

    tracing::info!(socket = ?state.socket_name, profile = ?state.bind_profile, log = ?log_path, "xross started");
    // Children (and anything spawned by binds) connect to us, not the host compositor.
    unsafe { std::env::set_var("WAYLAND_DISPLAY", &state.socket_name) };

    // As a session, tell systemd and D-Bus where our display is, so portals and
    // D-Bus activated apps connect to us. Not for a run from a TTY next to another
    // session: that would point the shared user manager away from it.
    if args.session {
        unsafe {
            std::env::set_var("XDG_SESSION_TYPE", "wayland");
            if std::env::var_os("XDG_CURRENT_DESKTOP").is_none() {
                std::env::set_var("XDG_CURRENT_DESKTOP", "xross");
            }
        }
        state.pending_startup_commands.insert(
            0,
            "dbus-update-activation-environment --systemd WAYLAND_DISPLAY DISPLAY XDG_CURRENT_DESKTOP \
             XDG_SESSION_TYPE XDG_SESSION_DESKTOP"
                .into(),
        );
    }
    // Startup commands run once Xwayland is ready (or failed), so they inherit DISPLAY.
    state.pending_startup_commands.extend(args.command);
    let ipc_path = ipc::init(&mut state);
    state.start_xwayland();

    let res = event_loop.run(None, &mut state, |_| {});
    if let Some(path) = ipc_path {
        let _ = std::fs::remove_file(path);
    }
    res?;
    Ok(())
}

/// Log to stdout and to a file (`--log PATH`, else `/xross/xross.log`).
/// The previous run's log is kept as `xross.log.old`. Returns the file's path.
fn init_logging(path: Option<std::path::PathBuf>) -> Option<std::path::PathBuf> {
    use tracing_subscriber::{EnvFilter, fmt, prelude::*};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("xross=info,smithay=warn"));
    let file = path.or_else(default_log_path).and_then(|p| match open_log(&p) {
        Ok(f) => Some((p, f)),
        Err(err) => {
            eprintln!("xross: can't open log file {}: {err}", p.display());
            None
        }
    });
    let (log_path, file_layer) = match file {
        Some((p, f)) => (Some(p), Some(fmt::layer().with_ansi(false).with_writer(std::sync::Mutex::new(f)))),
        None => (None, None),
    };
    tracing_subscriber::registry().with(filter).with(fmt::layer()).with(file_layer).init();

    // A crash on the TTY would otherwise leave no trace once the screen is gone.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("panic: {info}\n{}", std::backtrace::Backtrace::force_capture());
        default_hook(info);
    }));
    log_path
}

fn default_log_path() -> Option<std::path::PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/state")))?;
    Some(state.join("xross").join("xross.log"))
}

/// Create the log file, keeping the previous one as `<name>.old`.
fn open_log(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if path.exists() {
        let _ = std::fs::rename(path, path.with_extension("log.old"));
    }
    std::fs::File::create(path)
}
