//! i3-awm — remember which i3 workspaces, resolution, position and primary flag
//! belonged to each monitor, and restore them when the monitor reconnects.
//!
//! Two threads share the [`Memory`] behind a mutex:
//!   * the i3 tracker keeps each connected output's workspace list current, so
//!     the "which workspaces were here" info survives i3's automatic re-homing
//!     at disconnect time;
//!   * the RandR watcher snapshots geometry on every layout change and restores
//!     an output the moment it reconnects.

mod i3;
mod memory;
mod randr_events;
mod xrandr;

use memory::Memory;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

type SharedMemory = Arc<Mutex<Memory>>;

fn main() {
    let mem: SharedMemory = Arc::new(Mutex::new(Memory::load()));

    // Baseline: record the current live layout without restoring anything, and
    // seed the set of connected outputs so nothing counts as "newly connected"
    // on the first reconcile.
    let mut prev_connected: HashSet<String> = xrandr::connected_names().into_iter().collect();
    snapshot(&mem);
    update_workspaces(&mem, &prev_connected);
    mem.lock().unwrap().save();
    println!(
        "i3-awm: started; tracking {} connected output(s)",
        prev_connected.len()
    );

    // Thread A: keep workspace lists up to date from i3 workspace events.
    {
        let mem = Arc::clone(&mem);
        thread::spawn(move || track_i3(mem));
    }

    // Thread B (this thread): react to monitor connect/disconnect.
    randr_events::watch(|| reconcile(&mem, &mut prev_connected));

    // watch() only returns on a fatal X error.
    eprintln!("i3-awm: RandR watcher exited; shutting down");
}

/// Copy every active output's current resolution/position/primary into memory,
/// so the last-good geometry is always captured before a future disconnect.
fn snapshot(mem: &SharedMemory) {
    let outputs = xrandr::query();
    let mut m = mem.lock().unwrap();
    for o in &outputs {
        if o.connected && o.active {
            let e = m.entry(&o.name);
            if let Some(res) = o.resolution {
                e.resolution = Some(res);
            }
            if let Some(pos) = o.position {
                e.position = Some(pos);
            }
            e.primary = o.primary;
        }
    }
}

/// Refresh workspace lists for currently connected outputs only. Outputs absent
/// from i3's report are left untouched, preserving the last-good list for a
/// monitor that just disconnected (whose workspaces i3 has re-homed elsewhere).
fn update_workspaces(mem: &SharedMemory, connected: &HashSet<String>) {
    let mapping = match i3::workspaces_by_output() {
        Some(m) => m,
        None => return,
    };
    let mut by_output: std::collections::HashMap<String, Vec<String>> = Default::default();
    for (ws, output) in mapping {
        by_output.entry(output).or_default().push(ws);
    }
    let mut m = mem.lock().unwrap();
    for (output, list) in by_output {
        if connected.contains(&output) {
            m.entry(&output).workspaces = list;
        }
    }
}

/// Thread A body: subscribe to i3 workspace events and update memory on each.
/// Reconnects with a short backoff if the i3 socket drops (e.g. i3 restart).
fn track_i3(mem: SharedMemory) {
    use i3ipc::event::Event;
    use i3ipc::{I3EventListener, Subscription};

    loop {
        match I3EventListener::connect() {
            Ok(mut listener) => {
                if let Err(e) = listener.subscribe(&[Subscription::Workspace]) {
                    eprintln!("i3-awm: failed to subscribe to i3 events: {e}");
                } else {
                    for event in listener.listen() {
                        match event {
                            Ok(Event::WorkspaceEvent(_)) => {
                                let connected: HashSet<String> =
                                    xrandr::connected_names().into_iter().collect();
                                update_workspaces(&mem, &connected);
                                mem.lock().unwrap().save();
                            }
                            Ok(_) => {}
                            Err(e) => {
                                eprintln!("i3-awm: i3 event error: {e}");
                                break;
                            }
                        }
                    }
                }
            }
            Err(e) => eprintln!("i3-awm: cannot connect to i3 event socket: {e}"),
        }
        thread::sleep(Duration::from_secs(2));
    }
}

/// Thread B body: on every RandR change, snapshot geometry, then restore any
/// output that just became connected.
fn reconcile(mem: &SharedMemory, prev_connected: &mut HashSet<String>) {
    let outputs = xrandr::query();
    let connected_now: HashSet<String> = outputs
        .iter()
        .filter(|o| o.connected)
        .map(|o| o.name.clone())
        .collect();

    snapshot(mem);
    update_workspaces(mem, &connected_now);

    let newly: Vec<String> = connected_now.difference(prev_connected).cloned().collect();
    for name in &newly {
        println!("i3-awm: output {name} connected; restoring");
        restore_output(mem, &outputs, name);
    }
    for name in prev_connected.difference(&connected_now) {
        println!("i3-awm: output {name} disconnected; state remembered");
    }

    *prev_connected = connected_now;
    mem.lock().unwrap().save();
}

/// Apply resolution/position/primary for a freshly connected output, wait for
/// i3 to see it, then move its remembered workspaces back onto it.
fn restore_output(mem: &SharedMemory, outputs: &[xrandr::OutputInfo], name: &str) {
    let info = outputs.iter().find(|o| o.name == name);

    let (stored, workspaces) = {
        let m = mem.lock().unwrap();
        let s = m.get(name).cloned();
        let ws = s.as_ref().map(|s| s.workspaces.clone()).unwrap_or_default();
        (s, ws)
    };

    let mut args: Vec<String> = vec!["--output".into(), name.into()];

    // Mode: stored resolution, else the monitor's preferred (native) mode.
    let mode = stored
        .as_ref()
        .and_then(|s| s.resolution)
        .or_else(|| info.and_then(|i| i.preferred));
    match mode {
        Some((w, h)) => {
            args.push("--mode".into());
            args.push(format!("{w}x{h}"));
        }
        None => args.push("--auto".into()),
    }

    // Position: stored absolute position, else right of the current primary.
    match stored.as_ref().and_then(|s| s.position) {
        Some((x, y)) => {
            args.push("--pos".into());
            args.push(format!("{x}x{y}"));
        }
        None => {
            if let Some(primary) = current_primary(outputs)
                && primary != name
            {
                args.push("--right-of".into());
                args.push(primary);
            }
        }
    }

    if stored.as_ref().is_some_and(|s| s.primary) {
        args.push("--primary".into());
    }

    if !xrandr::apply(&args) {
        return;
    }

    // i3 processes the RandR change asynchronously; wait until it sees the
    // output active before moving workspaces onto it (bounded ~2s).
    for _ in 0..20 {
        if i3::output_active(name) {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }

    i3::move_workspaces_to_output(&workspaces, name);
}

/// Name of the currently active primary output, if any.
fn current_primary(outputs: &[xrandr::OutputInfo]) -> Option<String> {
    outputs
        .iter()
        .find(|o| o.primary && o.active)
        .map(|o| o.name.clone())
}
