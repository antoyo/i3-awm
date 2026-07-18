//! Thin wrapper over the `xrandr` CLI: parse `xrandr --query` for the current
//! monitor state, and run `xrandr` to apply a configuration. We deliberately
//! shell out rather than drive RandR CRTC config directly — it is simpler and
//! matches the mental model of the i3 config that already calls `xrandr`.

use std::process::Command;

#[derive(Debug, Clone)]
pub struct OutputInfo {
    pub name: String,
    /// A monitor is physically plugged in.
    pub connected: bool,
    /// The output currently has a mode/geometry (it is enabled).
    pub active: bool,
    pub primary: bool,
    /// Current mode `(w, h)` when active.
    pub resolution: Option<(u32, u32)>,
    /// Current absolute position `(x, y)` when active.
    pub position: Option<(i32, i32)>,
    /// The monitor's preferred (native) mode, marked with `+` by xrandr.
    pub preferred: Option<(u32, u32)>,
}

/// Run `xrandr --query` and parse it. Returns an empty vec on failure.
pub fn query() -> Vec<OutputInfo> {
    let out = match Command::new("xrandr").arg("--query").output() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("i3-awm: failed to run xrandr --query: {e}");
            return Vec::new();
        }
    };
    parse(&String::from_utf8_lossy(&out.stdout))
}

/// Names of all currently connected outputs.
pub fn connected_names() -> Vec<String> {
    query()
        .into_iter()
        .filter(|o| o.connected)
        .map(|o| o.name)
        .collect()
}

/// Parse the textual output of `xrandr --query`.
pub fn parse(text: &str) -> Vec<OutputInfo> {
    let mut outputs: Vec<OutputInfo> = Vec::new();
    for line in text.lines() {
        if line.starts_with("Screen ") {
            continue;
        }
        // Mode lines are indented; they belong to the most recent output.
        if line.starts_with([' ', '\t']) {
            if line.contains('+')
                && let Some(last) = outputs.last_mut()
                && last.preferred.is_none()
            {
                last.preferred = parse_mode_resolution(line);
            }
            continue;
        }

        // Output header line: `NAME connected|disconnected [primary] [WxH+X+Y] ...`
        let mut parts = line.split_whitespace();
        let name = match parts.next() {
            Some(n) => n.to_string(),
            None => continue,
        };
        let status = parts.next().unwrap_or("");
        if status != "connected" && status != "disconnected" {
            continue; // not an output header we understand
        }

        let mut info = OutputInfo {
            name,
            connected: status == "connected",
            active: false,
            primary: false,
            resolution: None,
            position: None,
            preferred: None,
        };
        for tok in parts {
            if tok == "primary" {
                info.primary = true;
            } else if let Some((w, h, x, y)) = parse_geometry(tok) {
                info.resolution = Some((w, h));
                info.position = Some((x, y));
                info.active = true;
            }
        }
        outputs.push(info);
    }
    outputs
}

/// Parse a geometry token like `1920x1080+0+0` or `1280x720-100+0`.
fn parse_geometry(tok: &str) -> Option<(u32, u32, i32, i32)> {
    let x_idx = tok.find('x')?;
    let w: u32 = tok[..x_idx].parse().ok()?;
    let rest = &tok[x_idx + 1..];
    // Height runs until the first sign that begins the X offset.
    let sign_idx = rest.find(['+', '-'])?;
    let h: u32 = rest[..sign_idx].parse().ok()?;
    let (x, y) = parse_two_offsets(&rest[sign_idx..])?;
    Some((w, h, x, y))
}

/// Parse two signed offsets packed like `+0+0`, `-100+0`, `+1920+-0`.
fn parse_two_offsets(s: &str) -> Option<(i32, i32)> {
    // The string starts with a sign; the second number begins at the next sign.
    let split = s
        .char_indices()
        .skip(1)
        .find(|&(_, c)| c == '+' || c == '-')
        .map(|(i, _)| i)?;
    Some((parse_signed(&s[..split])?, parse_signed(&s[split..])?))
}

fn parse_signed(s: &str) -> Option<i32> {
    let (sign, digits) = match s.as_bytes().first()? {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => (1, s),
    };
    Some(sign * digits.parse::<i32>().ok()?)
}

/// First token of a mode line, e.g. `   1920x1080  60.00 +` -> `(1920, 1080)`.
fn parse_mode_resolution(line: &str) -> Option<(u32, u32)> {
    let tok = line.split_whitespace().next()?;
    let x_idx = tok.find('x')?;
    let w: u32 = tok[..x_idx].parse().ok()?;
    // Height may carry a suffix such as `i` (interlaced); take leading digits.
    let h_digits: String = tok[x_idx + 1..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    Some((w, h_digits.parse().ok()?))
}

/// Run `xrandr` with the given args. Returns whether it succeeded.
pub fn apply(args: &[String]) -> bool {
    match Command::new("xrandr").args(args).status() {
        Ok(s) if s.success() => true,
        Ok(s) => {
            eprintln!("i3-awm: xrandr {:?} exited with {s}", args);
            false
        }
        Err(e) => {
            eprintln!("i3-awm: failed to run xrandr {:?}: {e}", args);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Screen 0: minimum 320 x 200, current 1280 x 720, maximum 16384 x 16384
eDP-1 connected primary 1280x720+0+0 (normal left inverted right x axis y axis) 309mm x 174mm
   1920x1080     60.00 +  59.97    59.96
   1280x720      60.00    59.99    59.86*   59.74
HDMI-1 disconnected (normal left inverted right x axis y axis)
DP-2 connected 1920x1080+1280+0 (normal left inverted right x axis y axis) 520mm x 290mm
   1920x1080     60.00 +
";

    #[test]
    fn parses_primary_active_output() {
        let outs = parse(SAMPLE);
        let edp = outs.iter().find(|o| o.name == "eDP-1").unwrap();
        assert!(edp.connected && edp.active && edp.primary);
        assert_eq!(edp.resolution, Some((1280, 720)));
        assert_eq!(edp.position, Some((0, 0)));
        assert_eq!(edp.preferred, Some((1920, 1080)));
    }

    #[test]
    fn parses_disconnected_and_secondary() {
        let outs = parse(SAMPLE);
        let hdmi = outs.iter().find(|o| o.name == "HDMI-1").unwrap();
        assert!(!hdmi.connected && !hdmi.active);

        let dp = outs.iter().find(|o| o.name == "DP-2").unwrap();
        assert!(dp.connected && dp.active && !dp.primary);
        assert_eq!(dp.resolution, Some((1920, 1080)));
        assert_eq!(dp.position, Some((1280, 0)));
    }

    #[test]
    fn parses_negative_offsets() {
        assert_eq!(parse_geometry("1280x720-100+0"), Some((1280, 720, -100, 0)));
        assert_eq!(parse_geometry("1920x1080+0+0"), Some((1920, 1080, 0, 0)));
    }
}
