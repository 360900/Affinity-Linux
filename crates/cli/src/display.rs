//! Display detection for the Wine DPI. The GUI reads its own window; the CLI
//! has none, so it asks the Wayland compositor or the X server directly.

use std::process::Command;

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols::xdg::xdg_output::zv1::client::{zxdg_output_manager_v1, zxdg_output_v1};

use affinity_linux_core::wine;

/// A monitor's physical resolution and its host scale factor.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Monitor {
    resolution: (u32, u32),
    scale: f32,
}

/// The Wine DPI for the detected display and a line describing how it was
/// chosen. With several monitors the highest DPI wins, since Wine has a
/// single global value.
pub fn detect_dpi() -> Option<(u32, String)> {
    let monitors = if affinity_linux_core::launch::wayland_session() {
        wayland_monitors()
    } else {
        x11_monitors()
    };
    let x = wine::detect_x_scaling();
    monitors
        .into_iter()
        .map(|monitor| {
            let dpi = wine::recommended_dpi(Some(monitor.resolution), monitor.scale, x);
            let (width, height) = monitor.resolution;
            let summary = format!(
                "{dpi} ({width}x{height} at {:.2}x scale, {})",
                monitor.scale,
                x.label()
            );
            (dpi, summary)
        })
        .max_by_key(|(dpi, _)| *dpi)
}

#[derive(Default)]
struct Output {
    mode: Option<(i32, i32)>,
    scale: i32,
    logical: Option<(i32, i32)>,
}

#[derive(Default)]
struct State {
    outputs: Vec<Output>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_output::WlOutput, usize> for State {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let output = &mut state.outputs[*index];
        match event {
            wl_output::Event::Mode {
                flags: WEnum::Value(flags),
                width,
                height,
                ..
            } if flags.contains(wl_output::Mode::Current) => {
                output.mode = Some((width, height));
            }
            wl_output::Event::Scale { factor } => output.scale = factor,
            _ => {}
        }
    }
}

impl Dispatch<zxdg_output_manager_v1::ZxdgOutputManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &zxdg_output_manager_v1::ZxdgOutputManagerV1,
        _: zxdg_output_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zxdg_output_v1::ZxdgOutputV1, usize> for State {
    fn event(
        state: &mut Self,
        _: &zxdg_output_v1::ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zxdg_output_v1::Event::LogicalSize { width, height } = event {
            state.outputs[*index].logical = Some((width, height));
        }
    }
}

/// Reads each output's mode and, through xdg-output, its logical size. Their
/// ratio is the fractional scale; wl_output alone only reports an integer.
fn wayland_monitors() -> Vec<Monitor> {
    let Ok(connection) = Connection::connect_to_env() else {
        return Vec::new();
    };
    let Ok((globals, mut queue)) = registry_queue_init::<State>(&connection) else {
        return Vec::new();
    };
    let handle = queue.handle();
    let mut state = State::default();

    let outputs: Vec<wl_output::WlOutput> = globals.contents().with_list(|list| {
        list.iter()
            .filter(|global| global.interface == "wl_output")
            .enumerate()
            .map(|(index, global)| {
                globals.registry().bind::<wl_output::WlOutput, _, _>(
                    global.name,
                    global.version.min(4),
                    &handle,
                    index,
                )
            })
            .collect()
    });
    state.outputs = outputs.iter().map(|_| Output::default()).collect();

    if let Ok(manager) =
        globals.bind::<zxdg_output_manager_v1::ZxdgOutputManagerV1, _, _>(&handle, 1..=3, ())
    {
        for (index, output) in outputs.iter().enumerate() {
            manager.get_xdg_output(output, &handle, index);
        }
    }
    if queue.roundtrip(&mut state).is_err() || queue.roundtrip(&mut state).is_err() {
        return Vec::new();
    }
    state.outputs.iter().filter_map(wayland_monitor).collect()
}

fn wayland_monitor(output: &Output) -> Option<Monitor> {
    let (width, height) = output.mode?;
    let resolution = (u32::try_from(width).ok()?, u32::try_from(height).ok()?);
    // The mode is untransformed and the logical size is rotated, so compare
    // the longer sides.
    let scale = match output.logical {
        Some((logical_width, logical_height)) if logical_width > 0 && logical_height > 0 => {
            width.max(height) as f32 / logical_width.max(logical_height) as f32
        }
        _ => output.scale.max(1) as f32,
    };
    Some(Monitor { resolution, scale })
}

/// Resolutions from xrandr, scale from the Xft.dpi resource that desktops set
/// for their global scale.
fn x11_monitors() -> Vec<Monitor> {
    if std::env::var_os("DISPLAY").is_none_or(|value| value.is_empty()) {
        return Vec::new();
    }
    let scale = command_stdout("xrdb", &["-query"])
        .and_then(|resources| xft_dpi(&resources))
        .map_or(1.0, |dpi| dpi / 96.0);
    command_stdout("xrandr", &["--current"])
        .map(|output| {
            xrandr_resolutions(&output)
                .into_iter()
                .map(|resolution| Monitor { resolution, scale })
                .collect()
        })
        .unwrap_or_default()
}

fn command_stdout(program: &str, args: &[&str]) -> Option<String> {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn xft_dpi(resources: &str) -> Option<f32> {
    resources.lines().find_map(|line| {
        let value = line.strip_prefix("Xft.dpi:")?;
        value.trim().parse::<f32>().ok().filter(|dpi| *dpi > 0.0)
    })
}

/// The current mode of each connected output, e.g.
/// `eDP-1 connected primary 2560x1600+0+0 (normal ...) 302mm x 189mm`.
fn xrandr_resolutions(output: &str) -> Vec<(u32, u32)> {
    output
        .lines()
        .filter(|line| line.contains(" connected"))
        .filter_map(|line| line.split_whitespace().find_map(parse_geometry))
        .collect()
}

fn parse_geometry(token: &str) -> Option<(u32, u32)> {
    let (size, _) = token.split_once('+')?;
    let (width, height) = size.split_once('x')?;
    Some((width.parse().ok()?, height.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_xrandr_connected_outputs() {
        let output = "Screen 0: minimum 8 x 8, current 4480 x 1440, maximum 32767 x 32767\n\
            eDP-1 connected primary 1920x1080+0+0 (normal left inverted right) 344mm x 194mm\n   \
            1920x1080     60.00*+\n\
            HDMI-1 disconnected (normal left inverted right x axis y axis)\n\
            DP-1 connected 2560x1440+1920+0 (normal left inverted right) 597mm x 336mm\n";
        assert_eq!(xrandr_resolutions(output), vec![(1920, 1080), (2560, 1440)]);
    }

    #[test]
    fn skips_connected_outputs_without_a_mode() {
        let output = "DP-2 connected (normal left inverted right x axis y axis)\n";
        assert!(xrandr_resolutions(output).is_empty());
    }

    #[test]
    fn reads_xft_dpi() {
        assert_eq!(xft_dpi("Xcursor.size:\t24\nXft.dpi:\t144\n"), Some(144.0));
        assert_eq!(xft_dpi("Xcursor.size:\t24\n"), None);
    }

    #[test]
    fn wayland_scale_from_logical_size() {
        let output = Output {
            mode: Some((3840, 2400)),
            scale: 3,
            logical: Some((1707, 1067)),
        };
        let monitor = wayland_monitor(&output).unwrap();
        assert_eq!(monitor.resolution, (3840, 2400));
        assert!((monitor.scale - 2.25).abs() < 0.01);

        let rotated = Output {
            mode: Some((2560, 1600)),
            scale: 2,
            logical: Some((1280, 2048)),
        };
        assert!((wayland_monitor(&rotated).unwrap().scale - 1.25).abs() < 0.01);

        let no_xdg = Output {
            mode: Some((2560, 1600)),
            scale: 2,
            logical: None,
        };
        assert_eq!(wayland_monitor(&no_xdg).unwrap().scale, 2.0);
    }
}
