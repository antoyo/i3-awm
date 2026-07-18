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
    let (conn, screen_num) = match x11rb::connect(None) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("i3-awm: cannot connect to the X server: {e}");
            return;
        }
    };
    let root = conn.setup().roots[screen_num].root;

    // RandR must be present and negotiated before select_input works.
    let version = conn
        .randr_query_version(1, 5)
        .map_err(x11rb::errors::ReplyError::from)
        .and_then(|c| c.reply());
    if let Err(e) = version {
        eprintln!("i3-awm: RandR extension unavailable: {e}");
        return;
    }

    let mask = NotifyMask::SCREEN_CHANGE | NotifyMask::OUTPUT_CHANGE | NotifyMask::CRTC_CHANGE;
    let selected = conn
        .randr_select_input(root, mask)
        .map_err(x11rb::errors::ReplyError::from)
        .and_then(|c| c.check());
    if let Err(e) = selected {
        eprintln!("i3-awm: failed to select RandR input: {e}");
        return;
    }
    let _ = conn.flush();

    loop {
        let event = match conn.wait_for_event() {
            Ok(e) => e,
            Err(e) => {
                eprintln!("i3-awm: X event stream error: {e}");
                return;
            }
        };
        match event {
            Event::RandrScreenChangeNotify(_) | Event::RandrNotify(_) => {
                // A single plug/unplug emits a burst of events; drain the queue
                // so we reconcile once against the settled state.
                while let Ok(Some(_)) = conn.poll_for_event() {}
                on_change();
            }
            _ => {}
        }
    }
}
