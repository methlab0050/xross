#![allow(irrefutable_let_patterns)]

mod anim;
mod backend;
mod capture;
mod cursor;
mod focus;
mod grabs;
mod handlers;
mod input;
mod ipc;
mod layout;
mod render;
mod screenshot;
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
}

const USAGE: &str = "usage: xross [--backend winit|udev] [--binds dev|prod] [--scale FACTOR] [-c|--command CMD]\n       xross msg ACTION...   (run an action in the running xross, e.g. `xross msg workspace right`)";

fn parse_args() -> Result<Args, String> {
    let mut args = Args { backend: Backend::Winit, binds: None, command: None, scale: None };
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
            "--binds" => args.binds = Some(value()?.parse()?),
            "-c" | "--command" => args.command = Some(value()?),
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

    init_logging();
    let args = parse_args()?;

    // Nested runs inside GNOME, which grabs Super, so it defaults to the Alt-based dev binds.
    let profile = args.binds.unwrap_or(match args.backend {
        Backend::Winit => Profile::Dev,
        Backend::Udev => Profile::Prod,
    });

    let mut event_loop: EventLoop<XrossState> = EventLoop::try_new()?;
    let display: Display<XrossState> = Display::new()?;
    let mut state = XrossState::new(&mut event_loop, display, profile);

    match args.backend {
        Backend::Winit => backend::winit::init_winit(&mut event_loop, &mut state)?,
        Backend::Udev => backend::udev::init_udev(&mut event_loop, &mut state, args.scale)?,
    }

    tracing::info!(socket = ?state.socket_name, ?profile, "xross started");
    // Children (and anything spawned by binds) connect to us, not the host compositor.
    unsafe { std::env::set_var("WAYLAND_DISPLAY", &state.socket_name) };

    // The startup command runs once Xwayland is ready (or failed), so it inherits DISPLAY.
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

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("xross=info,smithay=warn"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
