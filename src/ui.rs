//! Native, event-driven layer-shell launcher. All rendering is premultiplied ARGB software text.
use ab_glyph::{point, Font, FontArc, PxScale, ScaleFont};
use smithay_client_toolkit::reexports::{
    calloop::{EventLoop, LoopHandle},
    calloop_wayland_source::WaylandSource,
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_registry,
    delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use std::{error::Error, process::Command};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_seat, wl_shm, wl_surface},
    Connection, QueueHandle,
};

/// Show the launcher, returning the original input index, or None on cancellation.
pub fn choose(names: &[String]) -> Result<Option<usize>, Box<dyn Error>> {
    let font_path = Command::new("fc-match")
        .args(["-f", "%{file}", "monospace"])
        .output()?;
    if !font_path.status.success() {
        return Err("fc-match could not find a monospace font".into());
    }
    let path = String::from_utf8(font_path.stdout)?;
    let font = FontArc::try_from_vec(std::fs::read(path.trim())?)?;
    let conn = Connection::connect_to_env()?;
    let (globals, queue) = registry_queue_init(&conn)?;
    let mut event_loop: EventLoop<Launcher> = EventLoop::try_new()?;
    let qh = queue.handle();
    WaylandSource::new(conn.clone(), queue).insert(event_loop.handle())?;
    let compositor = CompositorState::bind(&globals, &qh)?;
    let shell = LayerShell::bind(&globals, &qh)?;
    let shm = Shm::bind(&globals, &qh)?;
    let layer = shell.create_layer_surface(
        &qh,
        compositor.create_surface(&qh),
        Layer::Overlay,
        Some("launcher"),
        None,
    );
    layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    layer.set_size(0, 0);
    layer.set_exclusive_zone(-1);
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    layer.commit();
    let pool = SlotPool::new(4096, &shm)?;
    let mut app = Launcher {
        loop_handle: event_loop.handle(),
        registry: RegistryState::new(&globals),
        seats: SeatState::new(&globals, &qh),
        outputs: OutputState::new(&globals, &qh),
        shm,
        pool,
        layer,
        font,
        names: names.to_vec(),
        query: String::new(),
        matches: Vec::new(),
        selected: 0,
        width: 0,
        height: 0,
        scale: 1,
        keyboard: None,
        modifiers: Modifiers::default(),
        done: false,
        answer: None,
        failure: None,
    };
    app.filter();
    while !app.done {
        event_loop.dispatch(None, &mut app)?;
    }
    if let Some(error) = app.failure {
        return Err(error.into());
    }
    Ok(app.answer)
}

struct Launcher {
    loop_handle: LoopHandle<'static, Launcher>,
    registry: RegistryState,
    seats: SeatState,
    outputs: OutputState,
    shm: Shm,
    pool: SlotPool,
    layer: LayerSurface,
    font: FontArc,
    names: Vec<String>,
    query: String,
    matches: Vec<(usize, Vec<usize>, i64)>,
    selected: usize,
    width: u32,
    height: u32,
    scale: i32,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    modifiers: Modifiers,
    done: bool,
    answer: Option<usize>,
    failure: Option<String>,
}

/// Case-insensitive Unicode subsequence match. Positions are character indices, never byte offsets.
fn fuzzy(name: &str, query: &str) -> Option<(Vec<usize>, i64)> {
    let hay: Vec<char> = name.chars().collect();
    let needle: Vec<char> = query.chars().collect();
    let mut positions = Vec::new();
    let mut next = 0;
    let mut score = 0;
    for wanted in needle {
        let found = (next..hay.len()).find(|&i| hay[i].to_lowercase().eq(wanted.to_lowercase()))?;
        score += (found - next) as i64 * 4;
        if positions.is_empty() {
            score += found as i64 * 3;
        }
        if found == 0 || !hay[found - 1].is_alphanumeric() {
            score -= 5;
        }
        positions.push(found);
        next = found + 1;
    }
    Some((positions, score))
}

impl Launcher {
    fn handle_key(&mut self, event: KeyEvent) {
        if self.done {
            return;
        }
        match event.keysym {
            Keysym::Escape => self.done = true,
            Keysym::Return | Keysym::KP_Enter => {
                self.answer = self.matches.get(self.selected).map(|m| m.0);
                if self.answer.is_some() {
                    self.done = true;
                }
            }
            Keysym::BackSpace => {
                self.query.pop();
                self.filter();
            }
            Keysym::u | Keysym::U if self.modifiers.ctrl => {
                self.query.clear();
                self.filter();
            }
            Keysym::Up | Keysym::ISO_Left_Tab => {
                if !self.matches.is_empty() {
                    self.selected = (self.selected + self.matches.len() - 1) % self.matches.len();
                }
            }
            Keysym::Down | Keysym::Tab => {
                if !self.matches.is_empty() {
                    self.selected = (self.selected + 1) % self.matches.len();
                }
            }
            _ => {
                if !self.modifiers.ctrl && !self.modifiers.alt && !self.modifiers.logo {
                    if let Some(text) = event.utf8 {
                        self.query.extend(text.chars().filter(|c| !c.is_control()));
                        self.filter();
                    }
                }
            }
        }
        self.draw();
    }
    fn filter(&mut self) {
        self.matches = self
            .names
            .iter()
            .enumerate()
            .filter_map(|(index, name)| {
                fuzzy(name, &self.query).map(|(positions, score)| (index, positions, score))
            })
            .collect();
        self.matches.sort_by_key(|entry| (entry.2, entry.0));
        self.selected = 0;
    }
    fn draw(&mut self) {
        if self.width == 0 || self.height == 0 || self.done {
            return;
        }
        if let Err(error) = self.render() {
            self.failure = Some(error.to_string());
            self.done = true;
        }
    }
    fn render(&mut self) -> Result<(), Box<dyn Error>> {
        let scale = self.scale.max(1) as u32;
        let width = self
            .width
            .checked_mul(scale)
            .ok_or("surface width overflow")?;
        let height = self
            .height
            .checked_mul(scale)
            .ok_or("surface height overflow")?;
        let stride = width
            .checked_mul(4)
            .filter(|&n| n <= i32::MAX as u32)
            .ok_or("surface stride overflow")?;
        if width > i32::MAX as u32 || height > i32::MAX as u32 {
            return Err("surface too large".into());
        }
        let (buffer, canvas) = self.pool.create_buffer(
            width as i32,
            height as i32,
            stride as i32,
            wl_shm::Format::Argb8888,
        )?;
        for pixel in canvas.chunks_exact_mut(4) {
            pixel.copy_from_slice(&0xaa000000u32.to_ne_bytes());
        }
        let font_size = 24.0 * scale as f32;
        let x = (width as f32 * 0.35) as i32;
        let y = (height as f32 * 0.35) as i32;
        let line = (font_size + 25.0 * scale as f32) as i32;
        let input = format!("> {}_", self.query);
        paint_text(
            canvas,
            width,
            height,
            &self.font,
            font_size,
            x,
            y,
            &input,
            0xffffff,
            &[],
        );
        if self.matches.is_empty() {
            paint_text(
                canvas,
                width,
                height,
                &self.font,
                font_size,
                x,
                y + line,
                "No matches",
                0xffffff,
                &[],
            );
        } else {
            let start = self.selected.saturating_sub(4);
            for (row, (index, positions, _)) in self.matches.iter().skip(start).take(5).enumerate()
            {
                let color = if start + row == self.selected {
                    0xff6347
                } else {
                    0xffffff
                };
                paint_text(
                    canvas,
                    width,
                    height,
                    &self.font,
                    font_size,
                    x,
                    y + line * (row as i32 + 1),
                    &self.names[*index],
                    color,
                    positions,
                );
            }
        }
        self.layer.wl_surface().set_buffer_scale(self.scale.max(1));
        self.layer
            .wl_surface()
            .damage_buffer(0, 0, width as i32, height as i32);
        buffer.attach_to(self.layer.wl_surface())?;
        self.layer.commit();
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_text(
    canvas: &mut [u8],
    width: u32,
    height: u32,
    font: &FontArc,
    size: f32,
    x: i32,
    y: i32,
    text: &str,
    color: u32,
    matches: &[usize],
) {
    let scaled = font.as_scaled(PxScale::from(size));
    let mut cursor = x as f32;
    let baseline = y as f32 + scaled.ascent();
    for (index, character) in text.chars().enumerate() {
        let id = scaled.glyph_id(character);
        let glyph = id.with_scale_and_position(size, point(cursor, baseline));
        let ink = if matches.contains(&index) {
            0xff4500
        } else {
            color
        };
        if let Some(outlined) = font.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            outlined.draw(|gx, gy, coverage| {
                let px = bounds.min.x as i32 + gx as i32;
                let py = bounds.min.y as i32 + gy as i32;
                if px < 0 || py < 0 || px >= width as i32 || py >= height as i32 {
                    return;
                }
                let offset = (py as usize * width as usize + px as usize) * 4;
                let dest = u32::from_ne_bytes(canvas[offset..offset + 4].try_into().unwrap());
                let a = (coverage * 255.0).round() as u32;
                let inverse = 255 - a;
                let mut result = (a + ((dest >> 24) * inverse + 127) / 255) << 24;
                for shift in [0, 8, 16] {
                    let channel =
                        (((ink >> shift) & 255) * a + ((dest >> shift) & 255) * inverse + 127)
                            / 255;
                    result |= channel << shift;
                }
                canvas[offset..offset + 4].copy_from_slice(&result.to_ne_bytes());
            });
        }
        cursor += scaled.h_advance(id);
        if cursor >= width as f32 {
            break;
        }
    }
}

impl CompositorHandler for Launcher {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        factor: i32,
    ) {
        self.scale = factor.max(1);
        self.draw();
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}
impl OutputHandler for Launcher {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}
impl LayerShellHandler for Launcher {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.done = true;
    }
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        config: LayerSurfaceConfigure,
        _: u32,
    ) {
        self.width = config.new_size.0.max(1);
        self.height = config.new_size.1.max(1);
        self.draw();
    }
}
impl SeatHandler for Launcher {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        cap: Capability,
    ) {
        if cap == Capability::Keyboard && self.keyboard.is_none() {
            match self.seats.get_keyboard_with_repeat(
                qh,
                &seat,
                None,
                self.loop_handle.clone(),
                Box::new(|app, _, event| app.handle_key(event)),
            ) {
                Ok(keyboard) => self.keyboard = Some(keyboard),
                Err(error) => {
                    self.failure = Some(error.to_string());
                    self.done = true;
                }
            }
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        cap: Capability,
    ) {
        if cap == Capability::Keyboard {
            if let Some(keyboard) = self.keyboard.take() {
                keyboard.release();
            }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}
impl KeyboardHandler for Launcher {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
    }
    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.handle_key(event);
    }
    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }
    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        modifiers: Modifiers,
        _: u32,
    ) {
        self.modifiers = modifiers;
    }
}
impl ShmHandler for Launcher {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}
impl ProvidesRegistryState for Launcher {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}
delegate_compositor!(Launcher);
delegate_output!(Launcher);
delegate_shm!(Launcher);
delegate_seat!(Launcher);
delegate_keyboard!(Launcher);
delegate_layer!(Launcher);
delegate_registry!(Launcher);

#[cfg(test)]
mod tests {
    use super::fuzzy;
    #[test]
    fn unicode_positions_are_characters() {
        assert_eq!(fuzzy("Éclair 🦀 Rust", "é🦀r").unwrap().0, vec![0, 7, 9]);
    }
    #[test]
    fn subsequences_and_empty() {
        assert!(fuzzy("Firefox", "ffx").is_some());
        assert!(fuzzy("Firefox", "z").is_none());
        assert_eq!(fuzzy("anything", "").unwrap().0, Vec::<usize>::new());
    }
    #[test]
    fn contiguous_match_ranks_first() {
        assert!(
            fuzzy("terminal", "term").unwrap().1 < fuzzy("the error manager", "term").unwrap().1
        );
    }
}
