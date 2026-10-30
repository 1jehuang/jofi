//! Native, event-driven layer-shell launcher. All rendering is premultiplied ARGB software text.
use ab_glyph::{point, Font, FontArc, PxScale, ScaleFont};
use smithay_client_toolkit::reexports::{
    calloop::{channel, EventLoop, LoopHandle},
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
use smithay_client_toolkit::{delegate_subcompositor, subcompositor::SubcompositorState};
use std::error::Error;
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_seat, wl_shm, wl_surface},
    Connection, QueueHandle,
};
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};

/// Show the launcher, returning the original input index, or None on cancellation.
pub fn choose(
    prepare_names: impl FnOnce() -> Result<Vec<String>, Box<dyn Error>>,
    pending_font: crate::font::Pending,
) -> Result<Option<usize>, Box<dyn Error>> {
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
    let subcompositor =
        SubcompositorState::bind(compositor.wl_compositor().clone(), &globals, &qh)?;
    let (subsurface, content) = subcompositor.create_subsurface(layer.wl_surface().clone(), &qh);
    // Synchronized child commits become visible atomically with the parent.
    let viewporter: WpViewporter = globals.bind(&qh, 1..=1, ())?;
    let viewport = viewporter.get_viewport(layer.wl_surface(), &qh, ());
    let content_viewport = viewporter.get_viewport(&content, &qh, ());
    let fractional_manager: Option<WpFractionalScaleManagerV1> = globals.bind(&qh, 1..=1, ()).ok();
    let fractional_scale = fractional_manager
        .as_ref()
        .map(|manager| manager.get_fractional_scale(layer.wl_surface(), &qh, ()));
    layer.commit();
    let pool = SlotPool::new(4096, &shm)?;
    conn.flush()?;
    crate::startup::mark("Wayland initialized and layer configure requested");
    let names = prepare_names()?;
    let search = crate::search::SearchIndex::new(&names);
    let font = pending_font.join().map_err(|_| "font loader panicked")??;
    let (clipboard_sender, clipboard_receiver) = channel::channel();
    event_loop
        .handle()
        .insert_source(clipboard_receiver, |event, _, app: &mut Launcher| {
            if let channel::Event::Msg(reply) = event {
                app.clipboard_reply(reply);
            }
        })?;
    let mut app = Launcher {
        loop_handle: event_loop.handle(),
        registry: RegistryState::new(&globals),
        seats: SeatState::new(&globals, &qh),
        outputs: OutputState::new(&globals, &qh),
        shm,
        pool,
        layer,
        subsurface,
        content,
        viewport,
        content_viewport,
        _fractional_scale: fractional_scale,
        fractional_scale: None,
        background_attached: false,
        font,
        names,
        search,
        query: crate::input::Query::default(),
        clipboard_sender,
        clipboard_pending: false,
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
    subsurface: wayland_client::protocol::wl_subsurface::WlSubsurface,
    content: wl_surface::WlSurface,
    viewport: WpViewport,
    content_viewport: WpViewport,
    _fractional_scale: Option<WpFractionalScaleV1>,
    fractional_scale: Option<u32>,
    background_attached: bool,
    font: FontArc,
    names: Vec<String>,
    search: crate::search::SearchIndex,
    query: crate::input::Query,
    clipboard_sender: channel::Sender<ClipboardReply>,
    clipboard_pending: bool,
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

enum ClipboardJob {
    Copy {
        text: String,
        cut: bool,
        revision: u64,
    },
    Paste {
        revision: u64,
    },
}
struct ClipboardReply {
    revision: u64,
    cut: bool,
    result: Result<Option<String>, String>,
}

/// Tofi's font-size is in points, whereas ab_glyph's scale is ascent minus
/// descent, not pixels per em. Match Pango's 96-DPI point conversion and its
/// separately rounded ascent/descent for logical line boxes.
fn text_metrics(font: &FontArc) -> (f32, f32, f32) {
    let size = font
        .pt_to_px_scale(24.0)
        .expect("valid font units per em")
        .y;
    let scaled = font.as_scaled(size);
    let ascent = scaled.ascent().ceil();
    let height = ascent + (-scaled.descent()).ceil();
    (size, ascent, height)
}

fn glyph_advance(font: &FontArc, size: f32, scale: f32, ch: char) -> f32 {
    let scaled = font.as_scaled(size);
    // Pango's default round-glyph-positions operates in logical pixels,
    // before the surface's fractional device scale is applied.
    (scaled.h_advance(scaled.glyph_id(ch)) / scale).round() * scale
}

/// Logical coordinates leave a small ink-bearing margin and clip to the output.
fn content_layout(
    width: u32,
    height: u32,
    text_width: f32,
    rows: usize,
    line_height: f32,
) -> (i32, i32, u32, u32) {
    let x = (width as f32 * 0.35) as u32;
    let y = (height as f32 * 0.35) as u32;
    let left = x.saturating_sub(2);
    let top = y.saturating_sub(2);
    let w = (text_width.ceil() as u32)
        .saturating_add(28)
        .min(width - left)
        .max(1);
    let h = ((rows as f32 * (line_height + 25.0) + line_height).ceil() as u32)
        .saturating_add(4)
        .min(height - top)
        .max(1);
    (left as i32, top as i32, w, h)
}

fn paint_caret(
    canvas: &mut [u8],
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    thickness: u32,
    caret_height: u32,
) {
    paint_rectangle(
        canvas,
        width,
        height,
        x,
        y,
        thickness,
        caret_height,
        0xffff4500,
    );
}

#[allow(clippy::too_many_arguments)]
fn paint_rectangle(
    canvas: &mut [u8],
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    rect_width: u32,
    rect_height: u32,
    color: u32,
) {
    let left = i64::from(x).max(0).min(i64::from(width));
    let right = (i64::from(x) + i64::from(rect_width))
        .max(0)
        .min(i64::from(width));
    let top = i64::from(y).max(0).min(i64::from(height));
    let bottom = (i64::from(y) + i64::from(rect_height))
        .max(0)
        .min(i64::from(height));
    for py in top..bottom {
        for px in left..right {
            let offset = (py as usize * width as usize + px as usize) * 4;
            canvas[offset..offset + 4].copy_from_slice(&color.to_ne_bytes());
        }
    }
}

impl Launcher {
    fn clipboard_request(&mut self, job: ClipboardJob) {
        // Bound repeated shortcuts to one operation and keep Wayland input and
        // rendering responsive even if the clipboard owner stops responding.
        if self.clipboard_pending {
            return;
        }
        self.clipboard_pending = true;
        let sender = self.clipboard_sender.clone();
        std::thread::spawn(move || {
            let reply = match job {
                ClipboardJob::Copy {
                    text,
                    cut,
                    revision,
                } => ClipboardReply {
                    revision,
                    cut,
                    result: crate::clipboard::copy(&text).map(|()| None),
                },
                ClipboardJob::Paste { revision } => ClipboardReply {
                    revision,
                    cut: false,
                    result: crate::clipboard::paste().map(Some),
                },
            };
            let _ = sender.send(reply);
        });
    }
    fn clipboard_reply(&mut self, reply: ClipboardReply) {
        self.clipboard_pending = false;
        if self.done {
            return;
        }
        let changed = match reply.result {
            Ok(Some(text)) => self.query.paste_if_current(reply.revision, &text),
            Ok(None) if reply.cut => self.query.cut_if_current(reply.revision),
            Ok(None) => false,
            Err(error) => {
                // In particular, never delete selected text when copying fails.
                eprintln!("jofi: clipboard: {error}");
                false
            }
        };
        if changed {
            self.filter();
        }
        self.draw();
    }
    fn handle_key(&mut self, event: KeyEvent) {
        if self.done {
            return;
        }
        match event.keysym {
            Keysym::Escape => self.done = true,
            Keysym::Return | Keysym::KP_Enter if self.clipboard_pending => {}
            Keysym::Return | Keysym::KP_Enter => {
                self.answer = self.matches.get(self.selected).map(|m| m.0);
                if self.answer.is_some() {
                    self.done = true;
                }
            }
            Keysym::a | Keysym::A if self.modifiers.ctrl || self.modifiers.alt => {
                self.query.select_all();
            }
            Keysym::c | Keysym::C | Keysym::x | Keysym::X
                if self.modifiers.ctrl || self.modifiers.alt =>
            {
                if let Some(text) = self.query.selection() {
                    self.clipboard_request(ClipboardJob::Copy {
                        text: text.into(),
                        cut: matches!(event.keysym, Keysym::x | Keysym::X),
                        revision: self.query.revision,
                    });
                }
            }
            Keysym::v | Keysym::V if self.modifiers.ctrl || self.modifiers.alt => {
                self.clipboard_request(ClipboardJob::Paste {
                    revision: self.query.revision,
                });
            }
            Keysym::BackSpace if self.modifiers.alt || self.modifiers.ctrl => {
                self.query.delete_word();
                self.filter();
            }
            Keysym::BackSpace | Keysym::Delete if self.query.selected => {
                self.query.clear();
                self.filter();
            }
            Keysym::BackSpace => {
                self.query.backspace();
                self.filter();
            }
            Keysym::u | Keysym::U if self.modifiers.ctrl => {
                self.query.clear();
                self.filter();
            }
            Keysym::Left | Keysym::Right | Keysym::Home | Keysym::End => self.query.deselect(),
            Keysym::k | Keysym::K if self.modifiers.ctrl || self.modifiers.logo => {
                if !self.matches.is_empty() {
                    self.selected = (self.selected + self.matches.len() - 1) % self.matches.len();
                }
            }
            Keysym::j | Keysym::J if self.modifiers.ctrl || self.modifiers.logo => {
                if !self.matches.is_empty() {
                    self.selected = (self.selected + 1) % self.matches.len();
                }
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
                        self.query.insert(&text);
                        self.filter();
                    }
                }
            }
        }
        self.draw();
    }
    fn filter(&mut self) {
        self.matches = self.search.filter(&self.query.text);
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
        crate::startup::mark("render begin");
        let scale = self
            .fractional_scale
            .map(|n| n as f32 / 120.0)
            .unwrap_or(self.scale.max(1) as f32);
        let (logical_size, _, line_height) = text_metrics(&self.font);
        let font_size = logical_size * scale;
        let start = self.selected.saturating_sub(4);
        let rows: Vec<&str> = if self.matches.is_empty() {
            vec!["No matches"]
        } else {
            self.matches
                .iter()
                .skip(start)
                .take(5)
                .map(|m| self.names[m.0].as_str())
                .collect()
        };
        let extent = std::iter::once(self.query.text.as_str())
            .chain(rows.iter().copied())
            .map(|text| {
                text.chars()
                    .map(|c| glyph_advance(&self.font, font_size, scale, c))
                    .sum::<f32>()
            })
            .fold(0.0_f32, f32::max);
        let (left, top, logical_width, logical_height) = content_layout(
            self.width,
            self.height,
            extent / scale,
            rows.len(),
            line_height,
        );
        let width = (logical_width as f32 * scale).round().max(1.0) as u32;
        let height = (logical_height as f32 * scale).round().max(1.0) as u32;
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
        canvas.fill(0);
        let x = (2.0 * scale).floor() as i32;
        let y = (2.0 * scale).floor() as i32;
        let line = (line_height + 25.0) * scale;
        let advance: f32 = self
            .query
            .text
            .chars()
            .map(|ch| glyph_advance(&self.font, font_size, scale, ch))
            .sum();
        if self.query.selected {
            paint_rectangle(
                canvas,
                width,
                height,
                x,
                y,
                advance.ceil() as u32,
                (line_height * scale).round() as u32,
                0xff315f88,
            );
        }
        paint_text(
            canvas,
            width,
            height,
            &self.font,
            font_size,
            scale,
            x,
            y,
            &self.query.text,
            0xffffff,
            &[],
        );
        let scaled = self.font.as_scaled(PxScale::from(font_size));
        paint_caret(
            canvas,
            width,
            height,
            x.saturating_add(advance.round() as i32),
            y,
            (3.0 * scale).round() as u32,
            (line_height * scale).round() as u32,
        );
        if self.matches.is_empty() {
            paint_text(
                canvas,
                width,
                height,
                &self.font,
                font_size,
                scale,
                x,
                y + line.round() as i32,
                "No matches",
                0xffffff,
                &[],
            );
        } else {
            let start = self.selected.saturating_sub(4);
            for (row, (index, positions, _)) in self.matches.iter().skip(start).take(5).enumerate()
            {
                let row_y = y + (line * (row as f32 + 1.0)).round() as i32;
                if start + row == self.selected {
                    // Tofi paints the selected background over the existing
                    // dimmer, with zero padding and the text's ink width.
                    let mut cursor = x as f32;
                    let mut ink_left = f32::INFINITY;
                    let mut ink_right = f32::NEG_INFINITY;
                    for ch in self.names[*index].chars() {
                        let id = scaled.glyph_id(ch);
                        let glyph = id.with_scale_and_position(
                            font_size,
                            point(cursor, row_y as f32 + scaled.ascent()),
                        );
                        if let Some(outline) = self.font.outline_glyph(glyph) {
                            let bounds = outline.px_bounds();
                            ink_left = ink_left.min(bounds.min.x);
                            ink_right = ink_right.max(bounds.max.x);
                        }
                        cursor += glyph_advance(&self.font, font_size, scale, ch);
                    }
                    if ink_right > ink_left {
                        paint_rectangle(
                            canvas,
                            width,
                            height,
                            ink_left.floor() as i32,
                            y + (line * (row as f32 + 1.0)).round() as i32,
                            (ink_right.ceil() - ink_left.floor()) as u32,
                            (line_height * scale).round() as u32,
                            0xaa000000,
                        );
                    }
                }
                let color = if start + row == self.selected {
                    0xff6347
                } else {
                    if (start + row) % 2 == 1 {
                        0xf0f0f0
                    } else {
                        0xffffff
                    }
                };
                paint_text(
                    canvas,
                    width,
                    height,
                    &self.font,
                    font_size,
                    scale,
                    x,
                    row_y,
                    &self.names[*index],
                    color,
                    positions,
                );
            }
        }
        self.content.set_buffer_scale(1);
        self.content_viewport
            .set_destination(logical_width as i32, logical_height as i32);
        self.subsurface.set_position(left, top);
        self.content
            .damage_buffer(0, 0, width as i32, height as i32);
        buffer.attach_to(&self.content)?;
        self.content.commit();
        self.viewport
            .set_destination(self.width as i32, self.height as i32);
        if !self.background_attached {
            let (background, pixel) = self.pool.create_buffer(1, 1, 4, wl_shm::Format::Argb8888)?;
            pixel[..4].copy_from_slice(&0xaa000000u32.to_ne_bytes());
            background.attach_to(self.layer.wl_surface())?;
            self.background_attached = true;
        }
        self.layer
            .wl_surface()
            .damage(0, 0, self.width as i32, self.height as i32);
        self.layer.commit();
        crate::startup::mark("content and background committed");
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
    scale: f32,
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
        cursor += glyph_advance(font, size, scale, character);
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
        surface: &wl_surface::WlSurface,
        factor: i32,
    ) {
        if surface != self.layer.wl_surface() {
            return;
        }
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
impl wayland_client::Dispatch<WpFractionalScaleV1, ()> for Launcher {
    fn event(
        state: &mut Self,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            state.fractional_scale = Some(scale.max(1));
            state.draw();
        }
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
delegate_subcompositor!(Launcher);
wayland_client::delegate_noop!(Launcher: ignore WpViewporter);
wayland_client::delegate_noop!(Launcher: ignore WpViewport);
wayland_client::delegate_noop!(Launcher: ignore WpFractionalScaleManagerV1);
delegate_compositor!(Launcher);
delegate_output!(Launcher);
delegate_shm!(Launcher);
delegate_seat!(Launcher);
delegate_keyboard!(Launcher);
delegate_layer!(Launcher);
delegate_registry!(Launcher);

#[cfg(test)]
mod tests {
    use super::{content_layout, glyph_advance, paint_caret, paint_text, text_metrics};
    use ab_glyph::{Font, ScaleFont};

    #[test]
    fn theme_uses_points_and_logical_pixel_glyph_advances() {
        let font = crate::font::load().unwrap();
        let (size, ascent, height) = text_metrics(&font);
        let px_per_em = size * font.units_per_em().unwrap() / font.height_unscaled();
        assert!((px_per_em - 32.0).abs() < 0.001);
        assert_eq!(ascent, font.as_scaled(size).ascent().ceil());
        assert_eq!(height, ascent + (-font.as_scaled(size).descent()).ceil());
        for scale in [1.0, 1.25, 1.5, 1.75, 2.0] {
            assert_eq!(
                glyph_advance(&font, size * scale, scale, 'M'),
                font.as_scaled(size).h_advance(font.glyph_id('M')).round() * scale
            );
        }
    }
    #[test]
    fn content_bounds_preserve_origin_and_clip_long_text() {
        let (x, y, w, h) = content_layout(1646, 1029, 200.0, 5, 44.0);
        assert_eq!((x + 2, y + 2), (576, 360));
        assert_eq!((w, h), (228, 393));
        assert!(w * h * 4 < 1646 * 1029);
        for (width, height) in [(1, 1), (3, 2), (100, 40), (1646, 1029)] {
            let (x, y, w, h) = content_layout(width, height, f32::MAX, 5, 44.0);
            assert!(w > 0 && h > 0);
            assert!(x as u32 + w <= width && y as u32 + h <= height);
        }
    }
    #[test]
    fn transparent_text_is_premultiplied_and_retains_theme() {
        let font = crate::font::load().unwrap();
        for (color, matches, ink) in [
            (0xffffff, vec![], 0xffffff),
            (0xff6347, vec![], 0xff6347),
            (0xffffff, vec![0], 0xff4500),
        ] {
            let mut pixels = vec![0; 64 * 40 * 4];
            paint_text(
                &mut pixels,
                64,
                40,
                &font,
                24.0,
                1.0,
                2,
                2,
                "M",
                color,
                &matches,
            );
            assert!(pixels
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| u32::from_ne_bytes(*p) == 0xff000000 | ink));
            for p in pixels.as_chunks::<4>().0 {
                let value = u32::from_ne_bytes(*p);
                let a = value >> 24;
                for shift in [0, 8, 16] {
                    assert!((value >> shift) & 255 <= a);
                }
            }
            assert_eq!(&pixels[..4], &[0; 4]);
        }
    }
    #[test]
    fn caret_is_three_pixels_wide_and_clips_safely() {
        let mut pixels = vec![0; 8 * 6 * 4];
        paint_caret(&mut pixels, 8, 6, 2, 1, 3, 4);
        let count = pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| u32::from_ne_bytes(**p) == 0xffff4500)
            .count();
        assert_eq!(count, 12);
        assert_eq!(
            u32::from_ne_bytes(pixels[40..44].try_into().unwrap()),
            0xffff4500
        );
        paint_caret(&mut pixels, 8, 6, -2, -1, 3, 2);
        paint_caret(&mut pixels, 8, 6, i32::MAX, i32::MAX, 3, 24);
    }
}
