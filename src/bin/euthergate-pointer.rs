use std::io::{self, BufRead};
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use wayland_client::{
    Connection, Dispatch, QueueHandle, delegate_noop,
    protocol::{wl_pointer, wl_registry, wl_seat},
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1,
    zwlr_virtual_pointer_v1::{self, ZwlrVirtualPointerV1},
};

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

struct State {
    seat: Option<wl_seat::WlSeat>,
    manager: Option<zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_seat" if state.seat.is_none() => {
                    state.seat = Some(registry.bind(name, version.min(9), qh, ()));
                }
                "zwlr_virtual_pointer_manager_v1" if state.manager.is_none() => {
                    state.manager = Some(registry.bind(name, version.min(2), qh, ()));
                }
                _ => {}
            }
        }
    }
}

delegate_noop!(State: ignore wl_seat::WlSeat);
delegate_noop!(State: ignore zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1);
delegate_noop!(State: ignore zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1);

fn main() -> Result<()> {
    let connection =
        Connection::connect_to_env().context("could not connect to the Forge Wayland display")?;
    let mut queue = connection.new_event_queue();
    let qh = queue.handle();
    connection.display().get_registry(&qh, ());
    let mut state = State {
        seat: None,
        manager: None,
    };
    queue
        .roundtrip(&mut state)
        .context("could not discover Forge Wayland globals")?;
    let manager = state
        .manager
        .as_ref()
        .context("Forge does not expose zwlr_virtual_pointer_manager_v1")?
        .clone();
    let pointer = manager.create_virtual_pointer(state.seat.as_ref(), &qh, ());
    queue
        .roundtrip(&mut state)
        .context("could not create the Forge virtual pointer")?;

    let started = Instant::now();
    let mut pressed = Vec::new();
    for line in io::stdin().lock().lines() {
        let line = line.context("could not read a virtual-pointer command")?;
        if line.trim().is_empty() {
            continue;
        }
        if !handle_command(&connection, &pointer, &started, &mut pressed, &line)? {
            break;
        }
    }
    release_buttons(&connection, &pointer, &started, &mut pressed)?;
    pointer.destroy();
    manager.destroy();
    connection.flush().ok();
    Ok(())
}

fn handle_command(
    connection: &Connection,
    pointer: &ZwlrVirtualPointerV1,
    started: &Instant,
    pressed: &mut Vec<u32>,
    line: &str,
) -> Result<bool> {
    let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
    match fields.as_slice() {
        ["move", x, y, width, height] => {
            let width = parse_extent(width)?;
            let height = parse_extent(height)?;
            pointer.motion_absolute(
                timestamp(started),
                parse_coordinate(x, width)?,
                parse_coordinate(y, height)?,
                width,
                height,
            );
            pointer.frame();
        }
        ["button", button, "pressed"] => {
            let button = parse_button(button)?;
            if !pressed.contains(&button) {
                pointer.button(timestamp(started), button, wl_pointer::ButtonState::Pressed);
                pointer.frame();
                pressed.push(button);
            }
        }
        ["button", button, "released"] => {
            let button = parse_button(button)?;
            if let Some(index) = pressed.iter().position(|pressed| *pressed == button) {
                pointer.button(
                    timestamp(started),
                    button,
                    wl_pointer::ButtonState::Released,
                );
                pointer.frame();
                pressed.swap_remove(index);
            }
        }
        ["wheel", dx, dy] => {
            let dx = dx
                .parse::<f64>()
                .context("invalid horizontal wheel delta")?;
            let dy = dy.parse::<f64>().context("invalid vertical wheel delta")?;
            pointer.axis_source(wl_pointer::AxisSource::Wheel);
            if dx != 0.0 {
                pointer.axis(
                    timestamp(started),
                    wl_pointer::Axis::HorizontalScroll,
                    dx / 12.0,
                );
            }
            if dy != 0.0 {
                pointer.axis(
                    timestamp(started),
                    wl_pointer::Axis::VerticalScroll,
                    dy / 12.0,
                );
            }
            pointer.frame();
        }
        ["release"] => release_buttons(connection, pointer, started, pressed)?,
        ["stop"] => return Ok(false),
        _ => return Err(anyhow!("invalid virtual-pointer command")),
    }
    connection
        .flush()
        .context("could not flush a Forge virtual-pointer event")?;
    Ok(true)
}

fn release_buttons(
    connection: &Connection,
    pointer: &ZwlrVirtualPointerV1,
    started: &Instant,
    pressed: &mut Vec<u32>,
) -> Result<()> {
    for button in pressed.drain(..) {
        pointer.button(
            timestamp(started),
            button,
            wl_pointer::ButtonState::Released,
        );
    }
    pointer.frame();
    connection
        .flush()
        .context("could not flush Forge pointer releases")
}

fn timestamp(started: &Instant) -> u32 {
    started.elapsed().as_millis() as u32
}

fn parse_extent(value: &str) -> Result<u32> {
    value
        .parse::<u32>()
        .ok()
        .filter(|value| *value > 0)
        .context("invalid pointer extent")
}

fn parse_coordinate(value: &str, extent: u32) -> Result<u32> {
    let value = value.parse::<f64>().context("invalid pointer coordinate")?;
    Ok(value
        .round()
        .clamp(0.0, f64::from(extent.saturating_sub(1))) as u32)
}

fn parse_button(value: &str) -> Result<u32> {
    match value {
        "0" => Ok(BTN_LEFT),
        "1" => Ok(BTN_MIDDLE),
        "2" => Ok(BTN_RIGHT),
        _ => Err(anyhow!("unsupported pointer button")),
    }
}
