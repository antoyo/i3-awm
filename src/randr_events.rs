//! Subscribe to X11 RandR events and invoke a callback whenever the monitor
//! layout changes. This is the only place we talk to X directly; everything
//! else goes through the `xrandr` CLI.

use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::randr::{ConnectionExt, NotifyMask};

/// Connect to X, select RandR change notifications on the root window, and call
/// `on_change` for every relevant event. Blocks forever; returns only on a
/// fatal X error (so the caller can decide whether to exit).
pub fn watch(mut on_change: impl FnMut()) {
    let (connection, screen_number) = match x11rb::connect(None) {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("i3-awm: cannot connect to the X server: {error}");
            return;
        }
    };
    let root_window = connection.setup().roots[screen_number].root;

    // RandR must be present and negotiated before select_input works.
    let version = connection
        .randr_query_version(1, 5)
        .map_err(x11rb::errors::ReplyError::from)
        .and_then(|cookie| cookie.reply());
    if let Err(error) = version {
        eprintln!("i3-awm: RandR extension unavailable: {error}");
        return;
    }

    let notify_mask =
        NotifyMask::SCREEN_CHANGE | NotifyMask::OUTPUT_CHANGE | NotifyMask::CRTC_CHANGE;
    let selection = connection
        .randr_select_input(root_window, notify_mask)
        .map_err(x11rb::errors::ReplyError::from)
        .and_then(|cookie| cookie.check());
    if let Err(error) = selection {
        eprintln!("i3-awm: failed to select RandR input: {error}");
        return;
    }
    let _ = connection.flush();

    loop {
        let event = match connection.wait_for_event() {
            Ok(event) => event,
            Err(error) => {
                eprintln!("i3-awm: X event stream error: {error}");
                return;
            }
        };
        match event {
            Event::RandrScreenChangeNotify(_) | Event::RandrNotify(_) => {
                // A single plug/unplug emits a burst of events; drain the queue
                // so we reconcile once against the settled state.
                while let Ok(Some(_)) = connection.poll_for_event() {}
                on_change();
            }
            _ => {}
        }
    }
}
