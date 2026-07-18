//! Applying a monitor configuration by shelling out to the `xrandr` CLI. This
//! runs once per change (enable/disable, set mode/position/primary), so its cost
//! is not on the hot path. Reading state is done in [`crate::randr`] instead.

use std::process::Command;

/// Run `xrandr` with the given arguments. Returns whether it succeeded.
pub fn apply(arguments: &[String]) -> bool {
    match Command::new("xrandr").args(arguments).status() {
        Ok(status) if status.success() => true,
        Ok(status) => {
            eprintln!("i3-awm: xrandr {arguments:?} exited with {status}");
            false
        }
        Err(error) => {
            eprintln!("i3-awm: failed to run xrandr {arguments:?}: {error}");
            false
        }
    }
}
