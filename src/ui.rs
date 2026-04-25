use crate::desktop::DesktopEntry;
use crate::history::History;
use crate::launcher::{build_launch_command, launch};
use crate::search::{SearchIndex, SearchResult};
use crate::telemetry::Telemetry;
use anyhow::{Context, Result, bail};
use fontdue::layout::{CoordinateSystem, Layout, LayoutSettings, TextStyle};
use fontdue::{Font, FontSettings};
use serde::Serialize;
use serde_json::{Map, json};
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_registry,
    delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use std::fs;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Instant;
use wayland_client::{
    Connection, QueueHandle,
    protocol::{wl_keyboard, wl_output, wl_seat, wl_shm, wl_surface},
};

const NAMESPACE: &str = "jofi";
const DEFAULT_MAX_RESULTS: usize = 5;

#[derive(Debug, Clone, Serialize)]
pub struct UiOptions {
    pub font_path: Option<PathBuf>,
    pub background_alpha: u8,
    pub query_size_px: f32,
    pub result_size_px: f32,
    pub result_gap_px: f32,
    pub x_percent: f32,
    pub y_percent: f32,
    pub render_scale: u32,
    pub max_results: usize,
}

impl Default for UiOptions {
    fn default() -> Self {
        Self {
            font_path: None,
            // Matches tofi's #000A fullscreen background by default.
            background_alpha: 0xAA,
            query_size_px: 34.0,
            result_size_px: 28.0,
            result_gap_px: 25.0,
            x_percent: 0.35,
            y_percent: 0.35,
            render_scale: 2,
            max_results: DEFAULT_MAX_RESULTS,
        }
    }
}

pub fn run_launcher(
    index: SearchIndex,
    telemetry: Telemetry,
    options: UiOptions,
    history: History,
) -> Result<()> {
    let startup = Instant::now();
    let font_path = resolve_font_path(options.font_path.clone())?;
    let font_bytes = fs::read(&font_path)
        .with_context(|| format!("failed to read font {}", font_path.display()))?;
    let font = Font::from_bytes(font_bytes, FontSettings::default())
        .map_err(|err| anyhow::anyhow!("failed to parse font {}: {err}", font_path.display()))?;

    let conn = Connection::connect_to_env().context("failed to connect to Wayland compositor")?;
    let (globals, mut event_queue) = registry_queue_init(&conn)?;
    let qh = event_queue.handle();

    let compositor =
        CompositorState::bind(&globals, &qh).context("wl_compositor is unavailable")?;
    let layer_shell = LayerShell::bind(&globals, &qh).context("wlr layer-shell is unavailable")?;
    let shm = Shm::bind(&globals, &qh).context("wl_shm is unavailable")?;

    let surface = compositor.create_surface(&qh);
    let layer =
        layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some(NAMESPACE), None);
    layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    layer.set_exclusive_zone(-1);
    layer.set_size(0, 0);
    layer.commit();

    let pool = SlotPool::new(4, &shm).context("failed to create Wayland shm pool")?;

    let mut app = LauncherApp {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        shm,
        pool,
        layer,
        keyboard: None,
        keyboard_focus: false,
        width: 1,
        height: 1,
        first_configure: true,
        exit: false,
        index,
        query: String::new(),
        results: Vec::new(),
        selected: 0,
        font,
        font_path,
        options,
        history,
        telemetry,
        started_at: startup,
        last_draw_ns: 0,
    };
    app.refresh_results();

    app.telemetry.event("ui.start", {
        let mut fields = Map::new();
        fields.insert("namespace".to_string(), json!(NAMESPACE));
        fields.insert("font_path".to_string(), json!(app.font_path));
        fields.insert("entries".to_string(), json!(app.index.len()));
        fields
    });

    while !app.exit {
        event_queue.blocking_dispatch(&mut app)?;
    }

    app.telemetry.event("ui.exit", {
        let mut fields = Map::new();
        fields.insert(
            "uptime_ns".to_string(),
            json!(app.started_at.elapsed().as_nanos() as u64),
        );
        fields.insert("last_draw_ns".to_string(), json!(app.last_draw_ns));
        fields
    });

    Ok(())
}

struct LauncherApp {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    layer: LayerSurface,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    keyboard_focus: bool,
    width: u32,
    height: u32,
    first_configure: bool,
    exit: bool,
    index: SearchIndex,
    query: String,
    results: Vec<SearchResult>,
    selected: usize,
    font: Font,
    font_path: PathBuf,
    options: UiOptions,
    history: History,
    telemetry: Telemetry,
    started_at: Instant,
    last_draw_ns: u64,
}

impl LauncherApp {
    fn refresh_results(&mut self) {
        let mut span = self.telemetry.span("ui.search").field("query", &self.query);
        self.results = self
            .index
            .search(&self.query, self.options.max_results.max(1));
        if self.selected >= self.results.len() {
            self.selected = self.results.len().saturating_sub(1);
        }
        span.set_field("results", self.results.len());
    }

    fn selected_entry(&self) -> Option<&DesktopEntry> {
        self.results.get(self.selected).map(|result| &result.entry)
    }

    fn move_selection(&mut self, delta: isize) {
        if self.results.is_empty() {
            self.selected = 0;
            return;
        }
        let len = self.results.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(len) as usize;
    }

    fn launch_selected(&mut self) {
        let Some(entry) = self.selected_entry().cloned() else {
            self.exit = true;
            return;
        };
        let command = build_launch_command(&entry)
            .map(|cmd| cmd.as_vec())
            .unwrap_or_default();
        self.telemetry.event("ui.launch_selected", {
            let mut fields = Map::new();
            fields.insert("entry".to_string(), json!(entry.name));
            fields.insert("query".to_string(), json!(self.query));
            fields.insert("command".to_string(), json!(command));
            fields
        });
        self.history.increment(&entry.name);
        if let Err(err) = self.history.save() {
            self.telemetry.event("history.save_error", {
                let mut fields = Map::new();
                fields.insert("entry".to_string(), json!(entry.name));
                fields.insert("error".to_string(), json!(err.to_string()));
                fields
            });
        }
        if let Err(err) = launch(&entry) {
            self.telemetry.event("ui.launch_error", {
                let mut fields = Map::new();
                fields.insert("entry".to_string(), json!(entry.name));
                fields.insert("error".to_string(), json!(err.to_string()));
                fields
            });
        }
        self.exit = true;
    }

    fn handle_key(&mut self, qh: &QueueHandle<Self>, event: KeyEvent) {
        let ctrl = self.current_ctrl();
        match event.keysym {
            Keysym::Escape => self.exit = true,
            Keysym::Return => self.launch_selected(),
            Keysym::BackSpace => {
                self.query.pop();
                self.selected = 0;
                self.refresh_results();
                self.draw(qh);
            }
            Keysym::Up => {
                self.move_selection(-1);
                self.draw(qh);
            }
            Keysym::Down => {
                self.move_selection(1);
                self.draw(qh);
            }
            Keysym::h if ctrl => {
                self.query.pop();
                self.selected = 0;
                self.refresh_results();
                self.draw(qh);
            }
            Keysym::u if ctrl => {
                self.query.clear();
                self.selected = 0;
                self.refresh_results();
                self.draw(qh);
            }
            Keysym::c if ctrl => self.exit = true,
            Keysym::j if ctrl => {
                self.move_selection(1);
                self.draw(qh);
            }
            Keysym::k if ctrl => {
                self.move_selection(-1);
                self.draw(qh);
            }
            _ => {
                if ctrl {
                    return;
                }
                if let Some(text) = event.utf8.as_deref().filter(|text| is_printable_text(text)) {
                    self.query.push_str(text);
                    self.selected = 0;
                    self.refresh_results();
                    self.draw(qh);
                }
            }
        }
    }

    fn current_ctrl(&self) -> bool {
        // Updated through KeyboardHandler::update_modifiers. Stored indirectly by xkbcommon in SCTK's
        // keyboard state, but the handler passes the current value to us; keep a conservative fallback
        // through keyboard focus only in case modifiers haven't arrived yet.
        CTRL_ACTIVE.with(|ctrl| ctrl.get()) && self.keyboard_focus
    }

    fn draw(&mut self, _qh: &QueueHandle<Self>) {
        if self.width == 0 || self.height == 0 {
            return;
        }

        let start = Instant::now();
        let width = self.width;
        let height = self.height;
        let render_scale = self.options.render_scale.max(1);
        let buffer_width = width.saturating_mul(render_scale);
        let buffer_height = height.saturating_mul(render_scale);
        let stride = buffer_width as i32 * 4;
        let lines = self.visible_lines();
        let Ok((buffer, canvas)) = self.pool.create_buffer(
            buffer_width as i32,
            buffer_height as i32,
            stride,
            wl_shm::Format::Argb8888,
        ) else {
            self.exit = true;
            return;
        };

        fill_background(canvas, self.options.background_alpha);
        draw_lines(
            canvas,
            buffer_width,
            buffer_height,
            &self.font,
            &lines,
            LayoutSpec {
                x: (width as f32 * self.options.x_percent).round() * render_scale as f32,
                y: (height as f32 * self.options.y_percent).round() * render_scale as f32,
                scale: render_scale as f32,
            },
        );

        self.layer
            .wl_surface()
            .damage_buffer(0, 0, buffer_width as i32, buffer_height as i32);
        if buffer.attach_to(self.layer.wl_surface()).is_ok() {
            let _ = self.layer.set_buffer_scale(render_scale);
            self.layer.commit();
        }
        self.last_draw_ns = start.elapsed().as_nanos() as u64;
        self.telemetry.event("ui.draw", {
            let mut fields = Map::new();
            fields.insert("width".to_string(), json!(width));
            fields.insert("height".to_string(), json!(height));
            fields.insert("buffer_width".to_string(), json!(buffer_width));
            fields.insert("buffer_height".to_string(), json!(buffer_height));
            fields.insert("render_scale".to_string(), json!(render_scale));
            fields.insert("duration_ns".to_string(), json!(self.last_draw_ns));
            fields.insert("query_len".to_string(), json!(self.query.chars().count()));
            fields.insert("results".to_string(), json!(self.results.len()));
            fields
        });
    }

    fn visible_lines(&self) -> Vec<LineSpec> {
        let mut lines =
            Vec::with_capacity(self.results.len() + usize::from(!self.query.is_empty()));
        if !self.query.is_empty() {
            lines.push(LineSpec {
                text: self.query.clone(),
                size_px: self.options.query_size_px,
                color: Color::rgb(245, 247, 255),
                gap_after_px: self.options.result_size_px * 0.9,
            });
        }
        for (idx, result) in self.results.iter().enumerate() {
            lines.push(LineSpec {
                text: result.entry.name.clone(),
                size_px: self.options.result_size_px,
                color: if idx == self.selected {
                    Color::rgb(255, 99, 71)
                } else {
                    Color::rgb(255, 255, 255)
                },
                gap_after_px: self.options.result_gap_px,
            });
        }
        lines
    }
}

thread_local! {
    static CTRL_ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl CompositorHandler for LauncherApp {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_factor: i32,
    ) {
    }
    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }
    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
    }
    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for LauncherApp {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
}

impl LayerShellHandler for LauncherApp {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        self.width = NonZeroU32::new(configure.new_size.0).map_or(1920, NonZeroU32::get);
        self.height = NonZeroU32::new(configure.new_size.1).map_or(1080, NonZeroU32::get);
        if self.first_configure {
            self.first_configure = false;
            self.telemetry.event("ui.first_configure", {
                let mut fields = Map::new();
                fields.insert("width".to_string(), json!(self.width));
                fields.insert("height".to_string(), json!(self.height));
                fields.insert(
                    "startup_to_configure_ns".to_string(),
                    json!(self.started_at.elapsed().as_nanos() as u64),
                );
                fields
            });
        }
        self.draw(qh);
    }
}

impl SeatHandler for LauncherApp {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            match self.seat_state.get_keyboard(qh, &seat, None) {
                Ok(keyboard) => self.keyboard = Some(keyboard),
                Err(err) => {
                    self.telemetry.event("ui.keyboard_error", {
                        let mut fields = Map::new();
                        fields.insert("error".to_string(), json!(err.to_string()));
                        fields
                    });
                    self.exit = true;
                }
            }
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_some() {
            self.keyboard.take().unwrap().release();
        }
    }

    fn remove_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wl_seat::WlSeat) {
    }
}

impl KeyboardHandler for LauncherApp {
    fn enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _serial: u32,
        _raw: &[u32],
        _keysyms: &[Keysym],
    ) {
        if self.layer.wl_surface() == surface {
            self.keyboard_focus = true;
        }
    }

    fn leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _serial: u32,
    ) {
        if self.layer.wl_surface() == surface {
            self.keyboard_focus = false;
            CTRL_ACTIVE.with(|ctrl| ctrl.set(false));
        }
    }

    fn press_key(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        self.handle_key(qh, event);
    }

    fn repeat_key(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        self.handle_key(qh, event);
    }

    fn release_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        _event: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        modifiers: Modifiers,
        _raw_modifiers: RawModifiers,
        _layout: u32,
    ) {
        CTRL_ACTIVE.with(|ctrl| ctrl.set(modifiers.ctrl));
    }
}

impl ShmHandler for LauncherApp {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for LauncherApp {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(LauncherApp);
delegate_output!(LauncherApp);
delegate_shm!(LauncherApp);
delegate_seat!(LauncherApp);
delegate_keyboard!(LauncherApp);
delegate_layer!(LauncherApp);
delegate_registry!(LauncherApp);

#[derive(Debug, Clone)]
struct LineSpec {
    text: String,
    size_px: f32,
    color: Color,
    gap_after_px: f32,
}

#[derive(Debug, Clone, Copy)]
struct Color {
    r: u8,
    g: u8,
    b: u8,
}

impl Color {
    const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

#[derive(Debug, Clone, Copy)]
struct LayoutSpec {
    x: f32,
    y: f32,
    scale: f32,
}

fn draw_lines(
    canvas: &mut [u8],
    width: u32,
    _height: u32,
    font: &Font,
    lines: &[LineSpec],
    spec: LayoutSpec,
) {
    if lines.is_empty() {
        return;
    }

    let x = spec.x.round();
    let mut y = spec.y.round();
    let fonts = [font.clone()];

    for line in lines {
        y = y.round();
        draw_text(
            canvas,
            width,
            font,
            TextDraw {
                fonts: &fonts,
                text: &line.text,
                size_px: line.size_px * spec.scale,
                x,
                y,
                color: line.color,
            },
        );
        y += (line.size_px + line.gap_after_px) * spec.scale;
    }
}

struct TextDraw<'a> {
    fonts: &'a [Font; 1],
    text: &'a str,
    size_px: f32,
    x: f32,
    y: f32,
    color: Color,
}

fn draw_text(canvas: &mut [u8], width: u32, font: &Font, spec: TextDraw<'_>) {
    let mut layout = Layout::new(CoordinateSystem::PositiveYDown);
    layout.reset(&LayoutSettings {
        x: spec.x,
        y: spec.y,
        ..LayoutSettings::default()
    });
    layout.append(spec.fonts, &TextStyle::new(spec.text, spec.size_px, 0));

    let glyphs = layout.glyphs();
    if glyphs.is_empty() {
        return;
    }
    for glyph in glyphs {
        let (metrics, bitmap) = font.rasterize_config(glyph.key);
        let gx = glyph.x.round() as i32;
        let gy = glyph.y.round() as i32;
        draw_glyph(
            canvas,
            width,
            GlyphDraw {
                x: gx,
                y: gy,
                width: metrics.width,
                height: metrics.height,
                bitmap: &bitmap,
                color: spec.color,
            },
        );
    }
}

struct GlyphDraw<'a> {
    x: i32,
    y: i32,
    width: usize,
    height: usize,
    bitmap: &'a [u8],
    color: Color,
}

fn draw_glyph(canvas: &mut [u8], width: u32, glyph: GlyphDraw<'_>) {
    let height = canvas.len() / (width as usize * 4);
    for row in 0..glyph.height {
        let py = glyph.y + row as i32;
        if py < 0 || py >= height as i32 {
            continue;
        }
        for col in 0..glyph.width {
            let px = glyph.x + col as i32;
            if px < 0 || px >= width as i32 {
                continue;
            }
            let coverage = glyph.bitmap[row * glyph.width + col];
            if coverage == 0 {
                continue;
            }
            let idx = ((py as usize * width as usize) + px as usize) * 4;
            blend_pixel(&mut canvas[idx..idx + 4], glyph.color, coverage);
        }
    }
}

fn blend_pixel(dst: &mut [u8], color: Color, alpha: u8) {
    let sa = alpha as u32;
    let inv = 255 - sa;
    let sr = color.r as u32 * sa / 255;
    let sg = color.g as u32 * sa / 255;
    let sb = color.b as u32 * sa / 255;

    let db = dst[0] as u32;
    let dg = dst[1] as u32;
    let dr = dst[2] as u32;
    let da = dst[3] as u32;

    let out_b = sb + db * inv / 255;
    let out_g = sg + dg * inv / 255;
    let out_r = sr + dr * inv / 255;
    let out_a = sa + da * inv / 255;

    dst[0] = out_b.min(255) as u8;
    dst[1] = out_g.min(255) as u8;
    dst[2] = out_r.min(255) as u8;
    dst[3] = out_a.min(255) as u8;
}

fn fill_background(canvas: &mut [u8], alpha: u8) {
    for px in canvas.chunks_exact_mut(4) {
        px[0] = 0;
        px[1] = 0;
        px[2] = 0;
        px[3] = alpha;
    }
}

fn is_printable_text(text: &str) -> bool {
    text.chars().all(|ch| !ch.is_control())
}

fn resolve_font_path(configured: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = configured.or_else(|| std::env::var_os("JOFI_FONT").map(PathBuf::from)) {
        if path.is_file() {
            return Ok(path);
        }
        bail!("configured font path does not exist: {}", path.display());
    }

    let candidates = [
        "/usr/share/fonts/TTF/JetBrainsMonoNerdFont-Regular.ttf",
        "/usr/share/fonts/TTF/JetBrainsMonoNerdFontMono-Regular.ttf",
        "/usr/share/fonts/TTF/JetBrainsMonoNLNerdFont-Regular.ttf",
        "/usr/share/fonts/TTF/JetBrainsMono-Regular.ttf",
        "/usr/share/fonts/TTF/JetBrainsMonoNerdFont-Light.ttf",
        "/usr/share/fonts/noto/NotoSansMono-Regular.ttf",
        "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
    ];
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .ok_or_else(|| anyhow::anyhow!("no usable font found; pass --font or set JOFI_FONT"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printable_text_rejects_controls() {
        assert!(is_printable_text("abc"));
        assert!(!is_printable_text("\u{1b}"));
    }

    #[test]
    fn background_is_premultiplied_transparent_black() {
        let mut canvas = vec![255; 8];
        fill_background(&mut canvas, 123);
        assert_eq!(canvas, vec![0, 0, 0, 123, 0, 0, 0, 123]);
    }
}
