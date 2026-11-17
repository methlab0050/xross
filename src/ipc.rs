//! A tiny control socket: `xross msg <action>` runs an action as if its bind had been
//! pressed (see `Action`'s `FromStr` for the syntax). Useful for scripting, and for
//! driving the nested compositor in tests, where keys can't be injected.
//!
//! One datagram per command on `$XDG_RUNTIME_DIR/xross-<wayland socket>.sock`; children
//! of xross find it through `$XROSS_SOCKET`.

use std::{
    io::ErrorKind,
    os::unix::net::UnixDatagram,
    path::{Path, PathBuf},
};

use smithay::reexports::calloop::{Interest, Mode, PostAction, generic::Generic};

use crate::{input::binds::Action, state::XrossState};

pub const SOCKET_ENV: &str = "XROSS_SOCKET";

/// Bind the socket and export its path; returns the path so it can be removed on exit.
pub fn init(state: &mut XrossState) -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    let path = Path::new(&dir).join(format!("xross-{}.sock", state.socket_name.to_string_lossy()));
    let _ = std::fs::remove_file(&path);
    let socket = match UnixDatagram::bind(&path).and_then(|s| s.set_nonblocking(true).map(|_| s)) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(?path, %err, "failed to bind the control socket");
            return None;
        }
    };

    let res = state.loop_handle.insert_source(Generic::new(socket, Interest::READ, Mode::Level), |_, socket, state| {
        let mut buf = [0u8; 512];
        loop {
            match socket.recv(&mut buf) {
                Ok(n) => {
                    let msg = String::from_utf8_lossy(&buf[..n]);
                    if let Some(path) = msg.trim().strip_prefix("screenshot ") {
                        state.screenshot_request = Some(path.trim().into());
                        continue;
                    }
                    match msg.trim().parse::<Action>() {
                        Ok(action) => state.run_action(action),
                        Err(err) => tracing::warn!(%err, "bad control message"),
                    }
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) => {
                    tracing::warn!(%err, "control socket error");
                    break;
                }
            }
        }
        Ok(PostAction::Continue)
    });
    if let Err(err) = res {
        tracing::warn!(%err, "failed to watch the control socket");
        return None;
    }
    unsafe { std::env::set_var(SOCKET_ENV, &path) };
    tracing::info!(?path, "control socket ready");
    Some(path)
}

/// Client side of `xross msg`.
pub fn send(msg: &str) -> Result<(), String> {
    let mut msg = msg.to_string();
    if let Some(path) = msg.strip_prefix("screenshot ") {
        // The compositor may run elsewhere; give it an absolute path.
        let abs = std::path::absolute(path.trim()).map_err(|e| e.to_string())?;
        msg = format!("screenshot {}", abs.display());
    } else {
        msg.parse::<Action>()?;
    }
    let path =
        std::env::var_os(SOCKET_ENV).ok_or_else(|| format!("${SOCKET_ENV} is not set (run this inside xross)"))?;
    let socket = UnixDatagram::unbound().map_err(|e| e.to_string())?;
    socket.send_to(msg.as_bytes(), &path).map_err(|e| format!("{}: {e}", Path::new(&path).display()))?;
    Ok(())
}
