//! RandR over x11rb: query current output state and watch for changes, both on
//! a single long-lived X connection. Reading state this way costs about a
//! millisecond; shelling out to `xrandr --query` costs hundreds of milliseconds
//! on some systems, which dominated the latency of handling a monitor hotplug.
//!
//! Applying a configuration still goes through the `xrandr` CLI (see the
//! [`crate::xrandr`] module) — that path runs once per change and is not on the
//! hot path.

use std::collections::HashMap;
use std::thread;
use std::time::{Duration, Instant};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::randr::{self, ConnectionExt, NotifyMask};
use x11rb::protocol::xproto::Window;
use x11rb::rust_connection::RustConnection;

/// After the first event of a burst, wait this long with no further events
/// before reconciling, so the simultaneous events of a single plug/unplug
/// collapse into one reconcile. Kept small: a reconcile is now cheap (a
/// sub-millisecond RandR read), and `restore` only ever fires once per connect,
/// so there is no reason to sit through a long debounce before reacting.
const SETTLE: Duration = Duration::from_millis(80);
/// Never wait longer than this for a burst to go quiet, so we react promptly
/// even while a monitor is still emitting negotiation events.
const MAX_SETTLE: Duration = Duration::from_millis(400);

/// The current state of one output, as read from RandR.
#[derive(Debug, Clone)]
pub struct OutputInfo {
    pub name: String,
    /// A monitor is physically plugged in.
    pub connected: bool,
    /// The output currently has a mode/geometry (it is enabled).
    pub active: bool,
    pub primary: bool,
    /// Current mode `(width, height)` when active.
    pub resolution: Option<(u32, u32)>,
    /// Current absolute position `(x, y)` when active.
    pub position: Option<(i32, i32)>,
    /// The monitor's preferred (native) mode.
    pub preferred: Option<(u32, u32)>,
}

/// A live X connection with the RandR extension negotiated. Used both to read
/// output state and to wait for change notifications.
pub struct Randr {
    connection: RustConnection,
    root: Window,
}

impl Randr {
    /// Connect to X and negotiate RandR. Returns `None` (after logging) if X is
    /// unavailable or lacks RandR.
    pub fn connect() -> Option<Self> {
        let (connection, screen_number) = match x11rb::connect(None) {
            Ok(pair) => pair,
            Err(error) => {
                eprintln!("i3-awm: cannot connect to the X server: {error}");
                return None;
            }
        };
        let root = connection.setup().roots[screen_number].root;

        let version = connection
            .randr_query_version(1, 5)
            .map_err(x11rb::errors::ReplyError::from)
            .and_then(|cookie| cookie.reply());
        if let Err(error) = version {
            eprintln!("i3-awm: RandR extension unavailable: {error}");
            return None;
        }

        Some(Self { connection, root })
    }

    /// Read the current state of every output. Returns an empty vec on failure.
    pub fn query(&self) -> Vec<OutputInfo> {
        match self.try_query() {
            Ok(outputs) => outputs,
            Err(error) => {
                eprintln!("i3-awm: RandR query failed: {error}");
                Vec::new()
            }
        }
    }

    fn try_query(&self) -> Result<Vec<OutputInfo>, Box<dyn std::error::Error>> {
        let resources = self
            .connection
            .randr_get_screen_resources_current(self.root)?
            .reply()?;
        let config_timestamp = resources.config_timestamp;

        // Map each mode id to its pixel dimensions, to resolve preferred modes.
        let mode_dimensions: HashMap<u32, (u32, u32)> = resources
            .modes
            .iter()
            .map(|mode| (mode.id, (u32::from(mode.width), u32::from(mode.height))))
            .collect();

        let primary_output = self
            .connection
            .randr_get_output_primary(self.root)?
            .reply()?
            .output;

        // Pipeline the per-output info requests: send them all, then read the
        // replies, so we pay one round trip rather than one per output.
        let output_info_cookies: Vec<_> = resources
            .outputs
            .iter()
            .map(|&output| {
                self.connection
                    .randr_get_output_info(output, config_timestamp)
            })
            .collect();

        let mut outputs = Vec::with_capacity(resources.outputs.len());
        for (&output_id, cookie) in resources.outputs.iter().zip(output_info_cookies) {
            let info = match cookie
                .map_err(x11rb::errors::ReplyError::from)
                .and_then(|cookie| cookie.reply())
            {
                Ok(info) => info,
                Err(_) => continue,
            };

            let preferred = if info.num_preferred > 0 {
                info.modes
                    .first()
                    .and_then(|mode_id| mode_dimensions.get(mode_id).copied())
            } else {
                None
            };

            let mut output_info = OutputInfo {
                name: String::from_utf8_lossy(&info.name).into_owned(),
                connected: info.connection == randr::Connection::CONNECTED,
                active: false,
                primary: output_id == primary_output,
                resolution: None,
                position: None,
                preferred,
            };

            // A non-zero CRTC with a real mode means the output is enabled; read
            // its geometry from the CRTC.
            if info.crtc != 0
                && let Ok(crtc) = self
                    .connection
                    .randr_get_crtc_info(info.crtc, config_timestamp)
                && let Ok(crtc) = crtc.reply()
                && crtc.mode != 0
            {
                output_info.active = true;
                output_info.resolution = Some((u32::from(crtc.width), u32::from(crtc.height)));
                output_info.position = Some((i32::from(crtc.x), i32::from(crtc.y)));
            }

            outputs.push(output_info);
        }
        Ok(outputs)
    }

    /// Select RandR change notifications and call `on_change` once per settled
    /// burst of events. Blocks forever; returns only on a fatal X error.
    pub fn watch(&self, mut on_change: impl FnMut(&Randr)) {
        let notify_mask =
            NotifyMask::SCREEN_CHANGE | NotifyMask::OUTPUT_CHANGE | NotifyMask::CRTC_CHANGE;
        let selection = self
            .connection
            .randr_select_input(self.root, notify_mask)
            .map_err(x11rb::errors::ReplyError::from)
            .and_then(|cookie| cookie.check());
        if let Err(error) = selection {
            eprintln!("i3-awm: failed to select RandR input: {error}");
            return;
        }
        let _ = self.connection.flush();

        loop {
            let event = match self.connection.wait_for_event() {
                Ok(event) => event,
                Err(error) => {
                    eprintln!("i3-awm: X event stream error: {error}");
                    return;
                }
            };
            match event {
                Event::RandrScreenChangeNotify(_) | Event::RandrNotify(_) => {
                    // A single plug/unplug emits a burst of staggered events (EDID
                    // negotiation, CRTC setup), and our own reconcile's `xrandr`
                    // apply emits more. Wait for the burst to go quiet before
                    // reconciling once, so we don't run several reconciles back
                    // to back.
                    let burst_start = Instant::now();
                    loop {
                        thread::sleep(SETTLE);
                        let mut saw_more = false;
                        while let Ok(Some(_)) = self.connection.poll_for_event() {
                            saw_more = true;
                        }
                        if !saw_more || burst_start.elapsed() >= MAX_SETTLE {
                            break;
                        }
                    }
                    on_change(self);
                }
                _ => {}
            }
        }
    }
}
