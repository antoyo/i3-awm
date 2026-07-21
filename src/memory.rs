//! Persistent, in-memory record of what belonged to each monitor.
//!
//! Identity is the xrandr output name (e.g. `HDMI-1`). For every output we
//! remember its last-connected resolution, position, primary flag and the i3
//! workspaces that lived on it, so the layout can be restored on reconnect.
//! The whole thing is mirrored to a JSON file so it survives daemon restarts.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OutputState {
    /// Last-connected mode, `(width, height)`. `None` => use the monitor's
    /// preferred mode on restore.
    pub resolution: Option<(u32, u32)>,
    /// Last-connected absolute position, `(x, y)`. `None` => place `--right-of`
    /// the current primary on restore.
    pub position: Option<(i32, i32)>,
    /// Whether this output was the primary.
    pub primary: bool,
    /// i3 workspace names last seen on this output.
    pub workspaces: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Memory {
    pub outputs: BTreeMap<String, OutputState>,
}

impl Memory {
    /// Load the state file, or return an empty memory when it is missing or
    /// unparseable (a corrupt file must never stop the daemon).
    pub fn load() -> Self {
        let path = state_path();
        match std::fs::read_to_string(&path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_else(|error| {
                eprintln!(
                    "i3-awm: failed to parse {}: {error}; starting with empty memory",
                    path.display()
                );
                Memory::default()
            }),
            Err(_) => Memory::default(),
        }
    }

    /// Best-effort write to disk. Never panics; logs on failure.
    pub fn save(&self) {
        let path = state_path();
        if let Some(directory) = path.parent() {
            let _ = std::fs::create_dir_all(directory);
        }
        match serde_json::to_string_pretty(self) {
            Ok(serialized) => {
                if let Err(error) = std::fs::write(&path, serialized) {
                    eprintln!("i3-awm: failed to write {}: {error}", path.display());
                }
            }
            Err(error) => eprintln!("i3-awm: failed to serialize state: {error}"),
        }
    }

    pub fn entry(&mut self, name: &str) -> &mut OutputState {
        self.outputs.entry(name.to_string()).or_default()
    }

    pub fn get(&self, name: &str) -> Option<&OutputState> {
        self.outputs.get(name)
    }

    /// Mark `name` as the sole primary output, clearing the primary flag from
    /// every other remembered output so exactly one primary ever exists.
    pub fn set_primary(&mut self, name: &str) {
        for (output_name, state) in &mut self.outputs {
            state.primary = output_name == name;
        }
        self.entry(name).primary = true;
    }
}

/// `$XDG_STATE_HOME/i3-awm/state.json`, falling back to `~/.local/state`.
fn state_path() -> PathBuf {
    let base_directory = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| {
            let home_directory = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            home_directory.join(".local").join("state")
        });
    base_directory.join("i3-awm").join("state.json")
}
