//! i3 IPC helpers: read the workspace->output mapping and move workspaces onto
//! a specific output. Each helper makes its own short-lived connection so a
//! dropped socket never leaves the daemon wedged.

use i3ipc::I3Connection;

/// `(workspace_name, output_name)` for every current workspace, or `None` if
/// i3 is unreachable.
pub fn workspaces_by_output() -> Option<Vec<(String, String)>> {
    let mut connection = I3Connection::connect().ok()?;
    let workspaces_reply = connection.get_workspaces().ok()?;
    Some(
        workspaces_reply
            .workspaces
            .into_iter()
            .map(|workspace| (workspace.name, workspace.output))
            .collect(),
    )
}

/// Whether i3 currently reports `name` as an active output. Used to wait for i3
/// to catch up with an xrandr change before moving workspaces onto it.
pub fn output_active(name: &str) -> bool {
    let mut connection = match I3Connection::connect() {
        Ok(connection) => connection,
        Err(_) => return false,
    };
    match connection.get_outputs() {
        Ok(outputs_reply) => outputs_reply
            .outputs
            .iter()
            .any(|output| output.name == name && output.active),
        Err(_) => false,
    }
}

/// Name of the currently focused workspace, or `None` if i3 is unreachable.
pub fn focused_workspace() -> Option<String> {
    let mut connection = I3Connection::connect().ok()?;
    let reply = connection.get_workspaces().ok()?;
    reply
        .workspaces
        .into_iter()
        .find(|workspace| workspace.focused)
        .map(|workspace| workspace.name)
}

/// Move the named workspaces onto `output`, then focus `restore_focus`.
///
/// i3's `move workspace to output` acts on the focused workspace, so we focus
/// each one in turn. `restore_focus` must be captured by the caller *before*
/// the output was enabled: enabling an output makes i3 auto-create an empty
/// workspace on it, and focusing that phantom afterwards would resurrect it as
/// a spurious extra workspace.
pub fn move_workspaces_to_output(workspaces: &[String], output: &str, restore_focus: Option<&str>) {
    if workspaces.is_empty() {
        return;
    }
    let mut connection = match I3Connection::connect() {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("i3-awm: cannot connect to i3 to move workspaces: {error}");
            return;
        }
    };

    for workspace in workspaces {
        let command = format!(
            "workspace --no-auto-back-and-forth \"{workspace}\"; move workspace to output \"{output}\"",
            workspace = escape(workspace),
            output = escape(output),
        );
        if let Err(error) = connection.run_command(&command) {
            eprintln!("i3-awm: failed to move workspace {workspace} to {output}: {error}");
        }
    }

    if let Some(name) = restore_focus {
        let _ = connection.run_command(&format!(
            "workspace --no-auto-back-and-forth \"{}\"",
            escape(name)
        ));
    }
}

/// Escape characters that would break out of an i3 double-quoted string.
fn escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}
