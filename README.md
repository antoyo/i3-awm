# i3-awm

A small daemon that watches X11 monitor connect/disconnect events and restores
your [i3](https://i3wm.org/) layout per monitor. When a monitor is unplugged and
later plugged back in, `i3-awm` puts the workspaces that lived on it back on it,
and restores that monitor's resolution, position, and primary flag.

Monitors are identified by their **xrandr output name** (`HDMI-1`, `DP-1`,
`eDP-1`, …).

## What it remembers

For every output it has seen connected, `i3-awm` records:

- **Resolution** — the last mode the monitor was using.
- **Position** — the monitor's absolute `x/y` placement.
- **Primary** — whether it was the primary output.
- **Workspaces** — which i3 workspaces were on it.

This is kept both in memory and in a JSON file, so it survives daemon restarts
and reboots:

```
$XDG_STATE_HOME/i3-awm/state.json   # default: ~/.local/state/i3-awm/state.json
```

The file is human-readable and safe to hand-edit while the daemon is stopped.

## Behavior on reconnect

When an output becomes connected:

1. It is enabled with `xrandr`:
   - **resolution** = the remembered mode, or the monitor's **preferred (native)
     mode** if nothing is remembered;
   - **position** = the remembered position, or `--right-of` the current primary
     the first time a monitor is ever seen;
   - `--primary` if it was remembered as primary.
2. Once i3 sees the output as active, the remembered workspaces are moved back
   onto it.

When an output disconnects, i3 automatically moves its workspaces to a surviving
monitor; `i3-awm` simply keeps the last-known assignment/geometry in memory so it
can be restored later.

### Note on disconnected monitors

You **cannot** read a monitor's resolution while it is disconnected — a
disconnected output reports no modes or EDID, even if your i3 config previously
ran `xrandr --mode` on it. That is why `i3-awm` captures the resolution while the
monitor is connected and persists it, falling back to the monitor's native mode
when there is no stored value.

## Build

```sh
cargo build --release
```

Requires the `xrandr` and `i3` (i3 IPC) tools at runtime, and an X11 session with
the RandR extension.

## Run

Start it from your i3 config so it runs for the whole session:

```
# ~/.config/i3/config
exec --no-startup-id /path/to/i3-awm/target/release/i3-awm
```

It logs to stdout/stderr. There are no command-line options.

## How it works

Two threads share the remembered state:

- an **i3 tracker** subscribes to i3 workspace events and keeps each connected
  output's workspace list current — importantly it never clears an output that
  has vanished, so the "which workspaces were on the monitor that just died"
  information survives i3's automatic re-homing;
- a **RandR watcher** subscribes to X11 RandR events, snapshots each active
  output's geometry on every change, and restores an output as soon as it
  reconnects.
