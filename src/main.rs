//! i3-awm — remember which i3 workspaces, resolution, position and primary flag
//! belonged to each monitor, and restore them when the monitor reconnects.
//!
//! Two threads share the [`Memory`] behind a mutex:
//!   * the i3 tracker keeps each connected output's workspace list current, so
//!     the "which workspaces were here" info survives i3's automatic re-homing
//!     at disconnect time;
//!   * the RandR watcher snapshots geometry on every layout change, disables an
//!     output when it disconnects, and restores it the moment it reconnects.
//!
//! `xrandr --query` is surprisingly expensive on some systems (hundreds of ms),
//! so it is called at most once per reconcile: the RandR watcher queries once
//! and shares the resulting connected-output set with the i3 tracker, which
//! therefore never shells out to xrandr itself.

mod i3;
mod memory;
mod randr;
mod xrandr;

use memory::Memory;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

type SharedMemory = Arc<Mutex<Memory>>;
/// The set of currently connected output names, maintained by the RandR watcher
/// (Thread B) and read by the i3 tracker (Thread A) so the latter never has to
/// run its own `xrandr --query`.
type SharedConnected = Arc<Mutex<HashSet<String>>>;

fn main() {
    let randr = match randr::Randr::connect() {
        Some(randr) => randr,
        None => return,
    };

    let shared_memory: SharedMemory = Arc::new(Mutex::new(Memory::load()));

    // Baseline: record the current live layout without restoring anything, and
    // seed the set of connected outputs so nothing counts as "newly connected"
    // on the first reconcile.
    let outputs = randr.query();
    let mut previously_connected: HashSet<String> = outputs
        .iter()
        .filter(|output| output.connected)
        .map(|output| output.name.clone())
        .collect();
    let connected_cache: SharedConnected = Arc::new(Mutex::new(previously_connected.clone()));
    snapshot(&shared_memory, &outputs);
    update_workspaces(&shared_memory, &previously_connected);
    shared_memory.lock().unwrap().save();
    println!(
        "i3-awm: started; tracking {} connected output(s)",
        previously_connected.len()
    );

    // Thread A: keep workspace lists up to date from i3 workspace events.
    {
        let shared_memory = Arc::clone(&shared_memory);
        let connected_cache = Arc::clone(&connected_cache);
        thread::spawn(move || track_i3(shared_memory, connected_cache));
    }

    // Thread B (this thread): react to monitor connect/disconnect.
    randr.watch(|randr| {
        reconcile(
            randr,
            &shared_memory,
            &connected_cache,
            &mut previously_connected,
        )
    });

    // watch() only returns on a fatal X error.
    eprintln!("i3-awm: RandR watcher exited; shutting down");
}

/// Copy every active output's current resolution/position/primary into memory,
/// so the last-good geometry is always captured before a future disconnect.
/// Takes an already-queried output list to avoid a redundant `xrandr --query`.
fn snapshot(shared_memory: &SharedMemory, outputs: &[randr::OutputInfo]) {
    let mut memory = shared_memory.lock().unwrap();
    for output in outputs {
        if output.connected && output.active {
            let output_state = memory.entry(&output.name);
            if let Some(resolution) = output.resolution {
                output_state.resolution = Some(resolution);
            }
            if let Some(position) = output.position {
                output_state.position = Some(position);
            }
            output_state.primary = output.primary;
        }
    }
}

/// Refresh workspace lists for currently connected outputs only. Outputs absent
/// from i3's report are left untouched, preserving the last-good list for a
/// monitor that just disconnected (whose workspaces i3 has re-homed elsewhere).
fn update_workspaces(shared_memory: &SharedMemory, connected_outputs: &HashSet<String>) {
    let workspace_mapping = match i3::workspaces_by_output() {
        Some(mapping) => mapping,
        None => return,
    };
    let mut workspaces_per_output: std::collections::HashMap<String, Vec<String>> =
        Default::default();
    for (workspace_name, output_name) in workspace_mapping {
        workspaces_per_output
            .entry(output_name)
            .or_default()
            .push(workspace_name);
    }
    let mut memory = shared_memory.lock().unwrap();
    for (output_name, workspace_list) in workspaces_per_output {
        if connected_outputs.contains(&output_name) {
            memory.entry(&output_name).workspaces = workspace_list;
        }
    }
}

/// Thread A body: subscribe to i3 workspace events and update memory on each.
/// Reconnects with a short backoff if the i3 socket drops (e.g. i3 restart).
///
/// Reads the connected-output set from the shared cache rather than running
/// `xrandr --query`, which would otherwise add hundreds of milliseconds to the
/// handling of every single workspace event.
fn track_i3(shared_memory: SharedMemory, connected_cache: SharedConnected) {
    use i3ipc::event::Event;
    use i3ipc::{I3EventListener, Subscription};

    loop {
        match I3EventListener::connect() {
            Ok(mut listener) => {
                if let Err(error) = listener.subscribe(&[Subscription::Workspace]) {
                    eprintln!("i3-awm: failed to subscribe to i3 events: {error}");
                } else {
                    for event in listener.listen() {
                        match event {
                            Ok(Event::WorkspaceEvent(_)) => {
                                let connected_outputs = connected_cache.lock().unwrap().clone();
                                update_workspaces(&shared_memory, &connected_outputs);
                                shared_memory.lock().unwrap().save();
                            }
                            Ok(_) => {}
                            Err(error) => {
                                eprintln!("i3-awm: i3 event error: {error}");
                                break;
                            }
                        }
                    }
                }
            }
            Err(error) => eprintln!("i3-awm: cannot connect to i3 event socket: {error}"),
        }
        thread::sleep(Duration::from_secs(2));
    }
}

/// Thread B body: on every RandR change, snapshot geometry, disable any output
/// that just disconnected, and restore any output that just became connected.
fn reconcile(
    randr: &randr::Randr,
    shared_memory: &SharedMemory,
    connected_cache: &SharedConnected,
    previously_connected: &mut HashSet<String>,
) {
    let outputs = randr.query();
    let currently_connected: HashSet<String> = outputs
        .iter()
        .filter(|output| output.connected)
        .map(|output| output.name.clone())
        .collect();
    // Publish the connected set so Thread A does not need to query xrandr.
    *connected_cache.lock().unwrap() = currently_connected.clone();

    snapshot(shared_memory, &outputs);
    update_workspaces(shared_memory, &currently_connected);

    let newly_connected: Vec<String> = currently_connected
        .difference(previously_connected)
        .cloned()
        .collect();
    for output_name in &newly_connected {
        println!("i3-awm: output {output_name} connected; restoring");
        restore_output(shared_memory, &outputs, output_name);
    }

    let newly_disconnected: Vec<String> = previously_connected
        .difference(&currently_connected)
        .cloned()
        .collect();
    for output_name in &newly_disconnected {
        println!("i3-awm: output {output_name} disconnected; disabling and remembering state");
        disable_output(output_name);
    }

    *previously_connected = currently_connected;
    shared_memory.lock().unwrap().save();
}

/// Turn a disconnected output off in xrandr. When a monitor is unplugged the X
/// server often leaves its CRTC enabled at the old mode (a "ghost" output that
/// still occupies desktop space); explicitly disabling it reclaims that space.
fn disable_output(name: &str) {
    xrandr::apply(&["--output".into(), name.into(), "--off".into()]);
}

/// Apply resolution/position/primary for a freshly connected output, wait for
/// i3 to see it, then move its remembered workspaces back onto it.
fn restore_output(shared_memory: &SharedMemory, outputs: &[randr::OutputInfo], name: &str) {
    let output_info = outputs.iter().find(|output| output.name == name);

    let (stored_state, workspaces) = {
        let memory = shared_memory.lock().unwrap();
        let stored_state = memory.get(name).cloned();
        let workspaces = stored_state
            .as_ref()
            .map(|state| state.workspaces.clone())
            .unwrap_or_default();
        (stored_state, workspaces)
    };

    let mut arguments: Vec<String> = vec!["--output".into(), name.into()];

    // Mode: stored resolution, else the monitor's preferred (native) mode.
    let mode = stored_state
        .as_ref()
        .and_then(|state| state.resolution)
        .or_else(|| output_info.and_then(|info| info.preferred));
    match mode {
        Some((width, height)) => {
            arguments.push("--mode".into());
            arguments.push(format!("{width}x{height}"));
        }
        None => arguments.push("--auto".into()),
    }

    // Position: stored absolute position, else right of the current primary.
    match stored_state.as_ref().and_then(|state| state.position) {
        Some((x_offset, y_offset)) => {
            arguments.push("--pos".into());
            arguments.push(format!("{x_offset}x{y_offset}"));
        }
        None => {
            if let Some(primary_name) = current_primary(outputs)
                && primary_name != name
            {
                arguments.push("--right-of".into());
                arguments.push(primary_name);
            }
        }
    }

    if stored_state.as_ref().is_some_and(|state| state.primary) {
        arguments.push("--primary".into());
    }

    let apply_start = std::time::Instant::now();
    if !xrandr::apply(&arguments) {
        return;
    }
    eprintln!(
        "i3-awm: {name} xrandr apply {arguments:?} took {}ms",
        apply_start.elapsed().as_millis()
    );

    // i3 processes the RandR change asynchronously; wait until it sees the
    // output active before moving workspaces onto it (bounded ~2s).
    let wait_start = std::time::Instant::now();
    let mut polls = 0;
    for _ in 0..20 {
        if i3::output_active(name) {
            break;
        }
        polls += 1;
        thread::sleep(Duration::from_millis(100));
    }
    eprintln!(
        "i3-awm: {name} i3 saw output active after {}ms ({polls} poll(s))",
        wait_start.elapsed().as_millis()
    );

    let move_start = std::time::Instant::now();
    i3::move_workspaces_to_output(&workspaces, name);
    eprintln!(
        "i3-awm: {name} moved {} workspace(s) in {}ms",
        workspaces.len(),
        move_start.elapsed().as_millis()
    );
}

/// Name of the currently active primary output, if any.
fn current_primary(outputs: &[randr::OutputInfo]) -> Option<String> {
    outputs
        .iter()
        .find(|output| output.primary && output.active)
        .map(|output| output.name.clone())
}
