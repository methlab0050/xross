#![allow(irrefutable_let_patterns)]

mod backend;
mod grabs;
mod handlers;
mod input;
mod state;

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
}

const USAGE: &str = "usage: xross [--backend winit|udev] [--binds dev|prod] [-c|--command CMD]";

fn parse_args() -> Result<Args, String> {
    let mut args = Args { backend: Backend::Winit, binds: None, command: None };
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
        Backend::Udev => return Err("udev backend is not implemented yet (stage 2)".into()),
    }

    tracing::info!(socket = ?state.socket_name, ?profile, "xross started");
    // Children (and anything spawned by binds) connect to us, not the host compositor.
    unsafe { std::env::set_var("WAYLAND_DISPLAY", &state.socket_name) };

    if let Some(cmd) = args.command {
        if let Err(err) = std::process::Command::new("sh").arg("-c").arg(&cmd).spawn() {
            tracing::warn!(%cmd, %err, "failed to spawn startup command");
        }
    }

    event_loop.run(None, &mut state, |_| {})?;
    Ok(())
}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("xross=info,smithay=warn"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
