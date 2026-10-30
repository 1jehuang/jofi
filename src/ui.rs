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
use smithay_client_toolkit::{delegate_subcompositor, subcompositor::SubcompositorState};
use std::error::Error;
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_seat, wl_shm, wl_surface},
    Connection, QueueHandle,
};
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};

/// Show the launcher, returning the original input index, or None on cancellation.
pub fn choose(
    names: &[String],
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
    layer.commit();
    let pool = SlotPool::new(4096, &shm)?;
    conn.flush()?;
    let font = pending_font.join().map_err(|_| "font loader panicked")??;
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
        background_attached: false,
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
    subsurface: wayland_client::protocol::wl_subsurface::WlSubsurface,
    content: wl_surface::WlSurface,
    viewport: WpViewport,
    background_attached: bool,
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

/// Subsequence matches always outrank typo fallbacks, retaining their original scoring.
fn fuzzy(name: &str, query: &str) -> Option<(Vec<usize>, i64)> {
    subsequence(name, query).or_else(|| typo_match(name, query))
}

/// Case-insensitive Unicode subsequence match. Positions are character indices, never byte offsets.
fn subsequence(name: &str, query: &str) -> Option<(Vec<usize>, i64)> {
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

/// A conservative word-level edit fallback, including adjacent transpositions.
/// Short queries remain subsequence-only to avoid flooding the result list.
fn typo_match(name: &str, query: &str) -> Option<(Vec<usize>, i64)> {
    let needle: Vec<char> = query
        .chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect();
    if needle.len() < 4 {
        return None;
    }
    let limit = if needle.len() >= 8 { 2 } else { 1 };
    let chars: Vec<char> = name.chars().collect();
    let mut ranges = vec![(0, chars.len())];
    let mut start = 0;
    for end in 0..=chars.len() {
        if end == chars.len() || !chars[end].is_alphanumeric() {
            if start < end {
                ranges.push((start, end));
            }
            start = end + 1;
        }
    }
    ranges
        .into_iter()
        .filter_map(|(start, end)| {
            if (end - start).abs_diff(needle.len()) > limit {
                return None;
            }
            let word: Vec<char> = chars[start..end]
                .iter()
                .map(|c| c.to_lowercase().next().unwrap_or(*c))
                .collect();
            let distance = edit_distance(&word, &needle);
            if distance > limit {
                return None;
            }
            let positions = (start..end)
                .filter(|&i| needle.contains(&chars[i].to_lowercase().next().unwrap_or(chars[i])))
                .collect();
            Some((
                positions,
                i64::MAX / 2 + distance as i64 * 100 + start as i64,
            ))
        })
        .min_by_key(|entry| entry.1)
}

fn edit_distance(left: &[char], right: &[char]) -> usize {
    let mut previous_previous = Vec::new();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (i, a) in left.iter().enumerate() {
        let mut row = vec![i + 1; right.len() + 1];
        for (j, b) in right.iter().enumerate() {
            row[j + 1] = (previous[j + 1] + 1)
                .min(row[j] + 1)
                .min(previous[j] + usize::from(a != b));
            if i > 0 && j > 0 && *a == right[j - 1] && left[i - 1] == *b {
                row[j + 1] = row[j + 1].min(previous_previous[j - 1] + 1);
            }
        }
        previous_previous = previous;
        previous = row;
    }
    previous[right.len()]
}

/// Logical coordinates leave a small ink-bearing margin and clip to the output.
fn content_layout(width: u32, height: u32, text_width: f32, rows: usize) -> (i32, i32, u32, u32) {
    let x = (width as f32 * 0.35) as u32;
    let y = (height as f32 * 0.35) as u32;
    let left = x.saturating_sub(2);
    let top = y.saturating_sub(2);
    let w = (text_width.ceil() as u32)
        .saturating_add(28)
        .min(width - left)
        .max(1);
    let h = (rows as u32 * 49 + 28).min(height - top).max(1);
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
    let left = i64::from(x).max(0).min(i64::from(width));
    let right = (i64::from(x) + i64::from(thickness))
        .max(0)
        .min(i64::from(width));
    let top = i64::from(y).max(0).min(i64::from(height));
    let bottom = (i64::from(y) + i64::from(caret_height))
        .max(0)
        .min(i64::from(height));
    for py in top..bottom {
        for px in left..right {
            let offset = (py as usize * width as usize + px as usize) * 4;
            canvas[offset..offset + 4].copy_from_slice(&0xffff4500u32.to_ne_bytes());
        }
    }
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
        crate::startup::mark("render begin");
        let scale = self.scale.max(1) as u32;
        let font_size = 24.0 * scale as f32;
        let scaled = self.font.as_scaled(PxScale::from(font_size));
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
        let extent = std::iter::once(self.query.as_str())
            .chain(rows.iter().copied())
            .map(|text| {
                text.chars()
                    .map(|c| scaled.h_advance(scaled.glyph_id(c)))
                    .sum::<f32>()
            })
            .fold(0.0_f32, f32::max);
        let (left, top, logical_width, logical_height) =
            content_layout(self.width, self.height, extent / scale as f32, rows.len());
        let width = logical_width
            .checked_mul(scale)
            .ok_or("surface width overflow")?;
        let height = logical_height
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
        canvas.fill(0);
        let x = 2 * scale as i32;
        let y = 2 * scale as i32;
        let line = (font_size + 25.0 * scale as f32) as i32;
        paint_text(
            canvas,
            width,
            height,
            &self.font,
            font_size,
            x,
            y,
            &self.query,
            0xffffff,
            &[],
        );
        let scaled = self.font.as_scaled(PxScale::from(font_size));
        let advance: f32 = self
            .query
            .chars()
            .map(|ch| scaled.h_advance(scaled.glyph_id(ch)))
            .sum();
        paint_caret(
            canvas,
            width,
            height,
            x.saturating_add(advance.round() as i32),
            y,
            3 * scale,
            font_size.ceil() as u32,
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
        self.content.set_buffer_scale(self.scale.max(1));
        self.subsurface.set_position(left, top);
        self.content
            .damage_buffer(0, 0, width as i32, height as i32);
        buffer.attach_to(&self.content)?;
        self.content.commit();
        self.viewport
            .set_destination(self.width as i32, self.height as i32);
        if !self.background_attached {
            let (background, pixel) = self.pool.create_buffer(1, 1, 4, wl_shm::Format::Argb8888)?;
            pixel.copy_from_slice(&0xaa000000u32.to_ne_bytes());
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
delegate_compositor!(Launcher);
delegate_output!(Launcher);
delegate_shm!(Launcher);
delegate_seat!(Launcher);
delegate_keyboard!(Launcher);
delegate_layer!(Launcher);
delegate_registry!(Launcher);

#[cfg(test)]
mod tests {
    use super::{content_layout, fuzzy, paint_caret, paint_text, subsequence};
    #[test]
    fn content_bounds_preserve_origin_and_clip_long_text() {
        let (x, y, w, h) = content_layout(1646, 1029, 200.0, 5);
        assert_eq!((x + 2, y + 2), (576, 360));
        assert_eq!((w, h), (228, 273));
        assert!(w * h * 4 < 1646 * 1029);
        for (width, height) in [(1, 1), (3, 2), (100, 40), (1646, 1029)] {
            let (x, y, w, h) = content_layout(width, height, f32::MAX, 5);
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
            paint_text(&mut pixels, 64, 40, &font, 24.0, 2, 2, "M", color, &matches);
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
    fn transposed_typo_matches_app_and_word() {
        assert_eq!(fuzzy("Chrome", "chrmoe").unwrap().0, vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(
            fuzzy("Google Chrome", "chrmoe").unwrap().0,
            vec![7, 8, 9, 10, 11, 12]
        );
        assert!(fuzzy("Chrome", "zzzzzz").is_none());
        assert!(fuzzy("Chrome", "crm").is_some());
        assert!(fuzzy("Chrome", "xro").is_none());
    }
    #[test]
    fn subsequence_rank_is_preserved_and_beats_typos() {
        for (name, query) in [
            ("Chrome", "chrm"),
            ("Firefox", "ffx"),
            ("Terminal", "term"),
            ("Éclair 🦀 Rust", "é🦀r"),
        ] {
            assert_eq!(fuzzy(name, query), subsequence(name, query));
        }
        assert!(
            fuzzy("chrmoe subsequence", "chrmoe").unwrap().1 < fuzzy("Chrome", "chrmoe").unwrap().1
        );
    }
    #[test]
    fn typo_unicode_positions_remain_character_indices() {
        assert_eq!(
            fuzzy("🦀 Éclair", "écliar").unwrap().0,
            vec![2, 3, 4, 5, 6, 7]
        );
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
