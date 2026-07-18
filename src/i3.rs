//! i3 IPC helpers: read the workspace->output mapping and move workspaces onto
//! a specific output. Each helper makes its own short-lived connection so a
//! dropped socket never leaves the daemon wedged.

use i3ipc::I3Connection;

/// `(workspace_name, output_name)` for every current workspace, or `None` if
/// i3 is unreachable.
pub fn workspaces_by_output() -> Option<Vec<(String, String)>> {
    let mut conn = I3Connection::connect().ok()?;
    let ws = conn.get_workspaces().ok()?;
    Some(
        ws.workspaces
            .into_iter()
            .map(|w| (w.name, w.output))
            .collect(),
    )
}

/// Whether i3 currently reports `name` as an active output. Used to wait for i3
/// to catch up with an xrandr change before moving workspaces onto it.
pub fn output_active(name: &str) -> bool {
    let mut conn = match I3Connection::connect() {
        Ok(c) => c,
        Err(_) => return false,
    };
    match conn.get_outputs() {
        Ok(outs) => outs.outputs.iter().any(|o| o.name == name && o.active),
        Err(_) => false,
    }
}

/// Move the named workspaces onto `output`, preserving the originally focused
/// workspace. i3's `move workspace to output` acts on the focused workspace, so
/// we focus each one in turn, then restore focus.
pub fn move_workspaces_to_output(workspaces: &[String], output: &str) {
    if workspaces.is_empty() {
        return;
    }
    let mut conn = match I3Connection::connect() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("i3-awm: cannot connect to i3 to move workspaces: {e}");
            return;
        }
    };

    let focused = conn.get_workspaces().ok().and_then(|w| {
        w.workspaces
            .into_iter()
            .find(|x| x.focused)
            .map(|x| x.name)
    });

    for ws in workspaces {
        let cmd = format!(
            "workspace --no-auto-back-and-forth \"{ws}\"; move workspace to output \"{output}\"",
            ws = escape(ws),
            output = escape(output),
        );
        if let Err(e) = conn.run_command(&cmd) {
            eprintln!("i3-awm: failed to move workspace {ws} to {output}: {e}");
        }
    }

    if let Some(f) = focused {
        let _ = conn.run_command(&format!(
            "workspace --no-auto-back-and-forth \"{}\"",
            escape(&f)
        ));
    }
}

/// Escape characters that would break out of an i3 double-quoted string.
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}
