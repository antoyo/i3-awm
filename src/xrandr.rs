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
    /// Current mode `(width, height)` when active.
    pub resolution: Option<(u32, u32)>,
    /// Current absolute position `(x, y)` when active.
    pub position: Option<(i32, i32)>,
    /// The monitor's preferred (native) mode, marked with `+` by xrandr.
    pub preferred: Option<(u32, u32)>,
}

/// Run `xrandr --query` and parse it. Returns an empty vec on failure.
pub fn query() -> Vec<OutputInfo> {
    let command_output = match Command::new("xrandr").arg("--query").output() {
        Ok(output) => output,
        Err(error) => {
            eprintln!("i3-awm: failed to run xrandr --query: {error}");
            return Vec::new();
        }
    };
    parse(&String::from_utf8_lossy(&command_output.stdout))
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
                && let Some(last_output) = outputs.last_mut()
                && last_output.preferred.is_none()
            {
                last_output.preferred = parse_mode_resolution(line);
            }
            continue;
        }

        // Output header line: `NAME connected|disconnected [primary] [WxH+X+Y] ...`
        let mut tokens = line.split_whitespace();
        let name = match tokens.next() {
            Some(name) => name.to_string(),
            None => continue,
        };
        let status = tokens.next().unwrap_or("");
        if status != "connected" && status != "disconnected" {
            continue; // not an output header we understand
        }

        let mut output_info = OutputInfo {
            name,
            connected: status == "connected",
            active: false,
            primary: false,
            resolution: None,
            position: None,
            preferred: None,
        };
        for token in tokens {
            if token == "primary" {
                output_info.primary = true;
            } else if let Some((width, height, x_offset, y_offset)) = parse_geometry(token) {
                output_info.resolution = Some((width, height));
                output_info.position = Some((x_offset, y_offset));
                output_info.active = true;
            }
        }
        outputs.push(output_info);
    }
    outputs
}

/// Parse a geometry token like `1920x1080+0+0` or `1280x720-100+0`.
fn parse_geometry(token: &str) -> Option<(u32, u32, i32, i32)> {
    let separator_index = token.find('x')?;
    let width: u32 = token[..separator_index].parse().ok()?;
    let remainder = &token[separator_index + 1..];
    // Height runs until the first sign that begins the X offset.
    let sign_index = remainder.find(['+', '-'])?;
    let height: u32 = remainder[..sign_index].parse().ok()?;
    let (x_offset, y_offset) = parse_two_offsets(&remainder[sign_index..])?;
    Some((width, height, x_offset, y_offset))
}

/// Parse two signed offsets packed like `+0+0`, `-100+0`, `+1920+-0`.
fn parse_two_offsets(text: &str) -> Option<(i32, i32)> {
    // The string starts with a sign; the second number begins at the next sign.
    let split_index = text
        .char_indices()
        .skip(1)
        .find(|&(_, character)| character == '+' || character == '-')
        .map(|(index, _)| index)?;
    Some((
        parse_signed(&text[..split_index])?,
        parse_signed(&text[split_index..])?,
    ))
}

fn parse_signed(text: &str) -> Option<i32> {
    let (sign, digits) = match text.as_bytes().first()? {
        b'+' => (1, &text[1..]),
        b'-' => (-1, &text[1..]),
        _ => (1, text),
    };
    Some(sign * digits.parse::<i32>().ok()?)
}

/// First token of a mode line, e.g. `   1920x1080  60.00 +` -> `(1920, 1080)`.
fn parse_mode_resolution(line: &str) -> Option<(u32, u32)> {
    let token = line.split_whitespace().next()?;
    let separator_index = token.find('x')?;
    let width: u32 = token[..separator_index].parse().ok()?;
    // Height may carry a suffix such as `i` (interlaced); take leading digits.
    let height_digits: String = token[separator_index + 1..]
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .collect();
    Some((width, height_digits.parse().ok()?))
}

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
        let outputs = parse(SAMPLE);
        let edp_output = outputs.iter().find(|output| output.name == "eDP-1").unwrap();
        assert!(edp_output.connected && edp_output.active && edp_output.primary);
        assert_eq!(edp_output.resolution, Some((1280, 720)));
        assert_eq!(edp_output.position, Some((0, 0)));
        assert_eq!(edp_output.preferred, Some((1920, 1080)));
    }

    #[test]
    fn parses_disconnected_and_secondary() {
        let outputs = parse(SAMPLE);
        let hdmi_output = outputs.iter().find(|output| output.name == "HDMI-1").unwrap();
        assert!(!hdmi_output.connected && !hdmi_output.active);

        let secondary_output = outputs.iter().find(|output| output.name == "DP-2").unwrap();
        assert!(secondary_output.connected && secondary_output.active && !secondary_output.primary);
        assert_eq!(secondary_output.resolution, Some((1920, 1080)));
        assert_eq!(secondary_output.position, Some((1280, 0)));
    }

    #[test]
    fn parses_negative_offsets() {
        assert_eq!(parse_geometry("1280x720-100+0"), Some((1280, 720, -100, 0)));
        assert_eq!(parse_geometry("1920x1080+0+0"), Some((1920, 1080, 0, 0)));
    }
}
