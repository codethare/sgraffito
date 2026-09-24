//! Wayland glue: layer surface lifecycle, mode switching, frame submission.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_dispatch2, delegate_registry,
    output::{OutputHandler, OutputInfo, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{Capability, SeatHandler, SeatState},
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{
        Shm, ShmHandler,
        slot::{Buffer, SlotPool},
    },
};
use wayland_client::{
    Connection, Proxy, QueueHandle,
    protocol::{wl_output, wl_seat, wl_shm, wl_surface},
};

use crate::canvas::{
    Doc, Hint, OutputAnnotations, Overlay, Rect, TextBuffer, TextItem, TextOverlay,
};
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::ZwpTextInputManagerV3;
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::ZwpTextInputV3;

use crate::input::stepped_size;
use crate::render::{DamageRegion, Renderer, buffer_format, damage_box, transient_bounds};
use crate::store::{self, Store};

/// Built-in palette (`#rrggbb`).
pub const PALETTE: [&str; 5] = ["#e01b24", "#f6d32d", "#33d17a", "#3584e4", "#ffffff"];
/// Initial stroke width and the range `[`/`]` may adjust it within, in logical pixels.
pub const PEN_WIDTH: f32 = 3.0;
pub const PEN_WIDTH_MIN: f32 = 1.0;
pub const PEN_WIDTH_MAX: f32 = 16.0;
pub const PEN_WIDTH_STEP: f32 = 1.0;
/// Initial text size and the range `[`/`]` may adjust it within, in logical pixels.
pub const TEXT_SIZE: f32 = 18.0;
pub const TEXT_SIZE_MIN: f32 = 8.0;
pub const TEXT_SIZE_MAX: f32 = 72.0;
pub const TEXT_SIZE_STEP: f32 = 2.0;

pub(crate) const BTN_LEFT: u32 = 0x110;
/// Max shm buffers kept per output: the compositor may still hold the previous one.
const MAX_BUFFERS: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Locked,
    Edit,
}

impl Mode {
    fn layer(self) -> Layer {
        match self {
            Mode::Locked => Layer::Background,
            Mode::Edit => Layer::Overlay,
        }
    }

    fn keyboard(self) -> KeyboardInteractivity {
        match self {
            Mode::Locked => KeyboardInteractivity::None,
            Mode::Edit => KeyboardInteractivity::Exclusive,
        }
    }
}

/// A single command line from the control socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Toggle,
    Edit,
    Lock,
    Clear,
}

impl Command {
    pub fn parse(line: &str) -> Result<Self, String> {
        match line.trim() {
            "toggle" => Ok(Command::Toggle),
            "edit" => Ok(Command::Edit),
            "lock" => Ok(Command::Lock),
            "clear" => Ok(Command::Clear),
            "" => Err("empty command".into()),
            other => Err(format!("unknown command '{other}'")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Pen,
    Eraser,
    Text,
}

pub struct Output {
    output: wl_output::WlOutput,
    /// The `wl_output` name; it may only arrive after the surface was created.
    name: String,
    surface: wl_surface::WlSurface,
    layer: LayerSurface,
    width: u32,
    height: u32,
    scale: f32,
    configured: bool,
    pub(crate) dirty: bool,
    /// Only the transient overlay moved, so the next frame can damage just its box.
    pub(crate) transient_dirty: bool,
    /// Transient overlay box as drawn in the previous frame; it has to be erased next time.
    last_transient: Option<Rect>,
    /// Buffer index that holds the previous frame. A bounding-box damage is only valid on it.
    last_buffer: Option<usize>,
    buffers: Vec<Buffer>,
    buffer_size: (u32, u32),
    pub(crate) overlay: Overlay,
}

impl Output {
    /// Key used for annotations; falls back to `output-<id>` until the name arrives.
    pub(crate) fn bucket(&self, id: u32) -> String {
        if self.name.is_empty() {
            format!("output-{id}")
        } else {
            self.name.clone()
        }
    }
}

pub struct App {
    pub(crate) qh: QueueHandle<App>,
    pub(crate) connection: Connection,
    pub(crate) registry_state: RegistryState,
    compositor_state: CompositorState,
    layer_shell: LayerShell,
    output_state: OutputState,
    seat_state: SeatState,
    shm: Shm,
    pool: SlotPool,

    renderer: Renderer,
    pub(crate) doc: Doc,
    pub(crate) store: Store,
    path: PathBuf,

    pub(crate) mode: Mode,
    pub(crate) tool: Tool,
    pub(crate) color_idx: usize,
    /// Width new strokes are created with, in logical pixels.
    pub(crate) pen_width: f32,
    /// Size new text boxes are created with, in logical pixels.
    pub(crate) text_size: f32,
    /// Keyed by the `wl_output` protocol id, because proxy identity is unreliable here.
    pub(crate) outputs: HashMap<u32, Output>,
    pub(crate) keyboard_focus: Option<u32>,
    pub(crate) dirty: bool,
    exit: bool,

    // text-input-v3 state
    pub(crate) text_input_manager: Option<ZwpTextInputManagerV3>,
    pub(crate) text_input: Option<ZwpTextInputV3>,
    pub(crate) text_input_enabled: bool,
    pub(crate) im_commit: String,
    pub(crate) im_preedit: Option<String>,
    pub(crate) im_delete: Option<(u32, u32)>,
    /// Shm format of every buffer and whether frames need the R/B swap. Decided once from the
    /// compositor's advertised list, because all buffers of an output have to agree.
    buffer_format: Option<(wl_shm::Format, bool)>,
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        connection: Connection,
        qh: QueueHandle<App>,
        registry_state: RegistryState,
        compositor_state: CompositorState,
        layer_shell: LayerShell,
        output_state: OutputState,
        seat_state: SeatState,
        shm: Shm,
    ) -> anyhow::Result<Self> {
        let path = store::path().ok_or_else(|| {
            anyhow::anyhow!("need $XDG_DATA_HOME or $HOME to locate the annotation file")
        })?;
        let doc = store::load(&path);
        let pool = SlotPool::new(1 << 20, &shm)?;
        let mut app = Self {
            qh,
            connection,
            registry_state,
            compositor_state,
            layer_shell,
            output_state,
            seat_state,
            shm,
            pool,
            renderer: Renderer::new(),
            doc,
            store: Store::new(path.clone()),
            path,
            mode: Mode::Locked,
            tool: Tool::Pen,
            color_idx: 0,
            pen_width: PEN_WIDTH,
            text_size: TEXT_SIZE,
            outputs: HashMap::new(),
            keyboard_focus: None,
            dirty: false,
            exit: false,
            text_input_manager: None,
            text_input: None,
            text_input_enabled: false,
            im_commit: String::new(),
            im_preedit: None,
            im_delete: None,
            buffer_format: None,
        };
        app.bind_text_input_manager();
        app.sync_outputs();
        Ok(app)
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn request_exit(&mut self) {
        self.exit = true;
    }

    pub fn exiting(&self) -> bool {
        self.exit
    }

    /// Flush pending annotations before exiting.
    pub fn shutdown(&mut self) {
        if let Err(e) = self.store.flush(&self.doc) {
            log(&format!(
                "failed to write {} on exit: {e}",
                self.path.display()
            ));
        }
    }

    fn name_of(&self, output: &wl_output::WlOutput) -> String {
        self.output_state
            .info(output)
            .and_then(|i| i.name)
            .unwrap_or_default()
    }

    /// Create layer surfaces for outputs that do not have one yet (startup, hotplug).
    fn sync_outputs(&mut self) {
        let outputs: Vec<wl_output::WlOutput> = self.output_state.outputs().collect();
        for output in outputs {
            if !self.outputs.contains_key(&key_of(&output)) {
                self.create_surface(output);
            }
        }
    }

    fn create_surface(&mut self, output: wl_output::WlOutput) {
        let surface = self.compositor_state.create_surface(&self.qh);
        // The compositor has to be told the buffer scale, or it maps the buffer 1:1 and every
        // annotation lands scale times too far out and is clipped.
        let scale = output_scale(self.output_state.info(&output).as_ref());
        surface.set_buffer_scale(scale as i32);
        let hint = self.current_hint();
        let layer = self.layer_shell.create_layer_surface(
            &self.qh,
            surface.clone(),
            self.mode.layer(),
            Some("sgraffito"),
            Some(&output),
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(self.mode.keyboard());
        layer.set_size(0, 0);
        apply_input_region(&self.compositor_state, &surface, self.mode);
        // The first commit carries no buffer; wait for configure to tell us the size.
        layer.commit();
        let name = self.name_of(&output);
        log(&format!(
            "layer surface created for output {}",
            if name.is_empty() { "<unnamed>" } else { &name }
        ));
        self.outputs.insert(
            key_of(&output),
            Output {
                output,
                name,
                surface,
                layer,
                width: 0,
                height: 0,
                scale,
                configured: false,
                dirty: true,
                transient_dirty: false,
                last_transient: None,
                last_buffer: None,
                buffers: Vec::new(),
                buffer_size: (0, 0),
                overlay: Overlay {
                    hint,
                    ..Default::default()
                },
            },
        );
    }

    /// Switch mode: destroy and recreate every surface on the new layer.
    ///
    /// Protocol v2 offers `set_layer`, but entering edit mode needs a fresh map so the
    /// compositor hands us keyboard focus; recreating is the most portable trigger.
    pub(crate) fn set_mode(&mut self, mode: Mode) {
        if self.mode == mode {
            return;
        }
        if mode == Mode::Locked {
            self.end_text_edit();
        }
        self.mode = mode;
        let keys: Vec<u32> = self.outputs.keys().copied().collect();
        for key in keys {
            let Some(old) = self.outputs.remove(&key) else {
                continue;
            };
            let (was_configured, name, output) =
                (old.configured, old.name.clone(), old.output.clone());
            drop(old);
            self.create_surface(output);
            if let Some(out) = self.outputs.get_mut(&key) {
                out.configured = was_configured;
                out.name = name;
                out.dirty = true;
            }
        }
        log(&format!("mode switched to {mode:?}"));
        self.refresh_hint();
    }

    /// The edit-mode hint of the current mode, tool, colour and size.
    fn current_hint(&self) -> Option<Hint> {
        match self.mode {
            Mode::Locked => None,
            Mode::Edit => Some(Hint {
                tool: tool_label(self.tool),
                color: PALETTE[self.color_idx],
                size: self.active_size(),
            }),
        }
    }

    /// The size the active tool creates content with.
    fn active_size(&self) -> f32 {
        match self.tool {
            Tool::Text => self.text_size,
            Tool::Pen | Tool::Eraser => self.pen_width,
        }
    }

    /// `[` and `]` step the active tool's size, clamped to its range. The hint shows the value,
    /// so it is repainted (a whole-surface frame, like a tool or colour change).
    pub(crate) fn nudge_size(&mut self, dir: f32) {
        match self.tool {
            Tool::Text => {
                self.text_size = stepped_size(
                    self.text_size,
                    dir,
                    TEXT_SIZE_STEP,
                    TEXT_SIZE_MIN,
                    TEXT_SIZE_MAX,
                )
            }
            Tool::Pen | Tool::Eraser => {
                self.pen_width = stepped_size(
                    self.pen_width,
                    dir,
                    PEN_WIDTH_STEP,
                    PEN_WIDTH_MIN,
                    PEN_WIDTH_MAX,
                )
            }
        }
        self.refresh_hint();
    }

    /// Push the current hint into every output's transient overlay. Called when the mode,
    /// the tool or the colour changes.
    fn refresh_hint(&mut self) {
        let hint = self.current_hint();
        for out in self.outputs.values_mut() {
            out.overlay.hint = hint;
            out.dirty = true;
        }
        self.dirty = true;
    }

    pub(crate) fn set_tool(&mut self, tool: Tool) {
        if self.tool != tool {
            self.end_text_edit();
            self.tool = tool;
            log(&format!("tool switched to {tool:?}"));
            self.mark_all_dirty();
            self.refresh_hint();
        }
    }

    fn mark_all_dirty(&mut self) {
        self.dirty = true;
        for out in self.outputs.values_mut() {
            out.dirty = true;
        }
    }

    pub(crate) fn set_color(&mut self, idx: usize) {
        if idx < PALETTE.len() {
            self.color_idx = idx;
            self.refresh_hint();
        }
    }

    /// Write the text box being edited back into the document (empty boxes are dropped).
    pub(crate) fn end_text_edit(&mut self) {
        let mut changed = false;
        let keys: Vec<u32> = self.outputs.keys().copied().collect();
        for key in keys {
            let id = key;
            let Some(out) = self.outputs.get_mut(&key) else {
                continue;
            };
            let Some(edit) = out.overlay.text.take() else {
                continue;
            };
            out.overlay.eraser = None;
            // The caret disappears and the box may be committed: repaint this output whole.
            out.dirty = true;
            if edit.buffer.text.is_empty() {
                continue;
            }
            let bucket = out.bucket(id);
            let mut item = edit.item.clone();
            item.text = edit.buffer.text.clone();
            let ann = self.doc.outputs.entry(bucket).or_default();
            match edit.index {
                Some(idx) if idx < ann.texts.len() => ann.texts[idx] = item,
                _ => ann.texts.push(item),
            }
            changed = true;
        }
        if changed {
            self.store.mark_dirty(Instant::now());
        }
        self.dirty = true;
        self.sync_text_input();
    }

    pub(crate) fn begin_text_edit(&mut self, key: &u32, x: f32, y: f32) {
        let id = *key;
        let Some(out) = self.outputs.get(key) else {
            return;
        };
        let bucket = out.bucket(id);
        let existing = self
            .doc
            .outputs
            .get(&bucket)
            .and_then(|ann| ann.texts.iter().position(|t| t.bounds().contains(x, y)));
        let (item, index) = match existing {
            Some(idx) => (self.doc.outputs[&bucket].texts[idx].clone(), Some(idx)),
            None => (
                TextItem {
                    x,
                    y,
                    color: PALETTE[self.color_idx].to_string(),
                    size: self.text_size,
                    text: String::new(),
                },
                None,
            ),
        };
        if let Some(out) = self.outputs.get_mut(key) {
            out.overlay.text = Some(TextOverlay {
                buffer: TextBuffer::new(item.text.clone()),
                item,
                index,
                preedit: String::new(),
            });
            out.dirty = true;
        }
        self.dirty = true;
        self.sync_text_input();
    }

    /// Handle one command from the socket and return the response line.
    pub fn command(&mut self, cmd: &str) -> String {
        let parsed = match Command::parse(cmd) {
            Ok(parsed) => parsed,
            Err(e) => return format!("error: {e}"),
        };
        match parsed {
            Command::Toggle => {
                let next = match self.mode {
                    Mode::Locked => Mode::Edit,
                    Mode::Edit => Mode::Locked,
                };
                self.set_mode(next);
                "ok".into()
            }
            Command::Edit => {
                self.set_mode(Mode::Edit);
                "ok".into()
            }
            Command::Lock => {
                self.set_mode(Mode::Locked);
                "ok".into()
            }
            Command::Clear => {
                self.end_text_edit();
                self.doc.outputs.clear();
                let _ = self.connection.flush();
                match self.store.flush(&self.doc) {
                    Ok(()) => {
                        self.mark_all_dirty();
                        "ok".into()
                    }
                    Err(e) => format!("error: {e}"),
                }
            }
        }
    }

    /// Called once per event loop iteration: flush pending writes and redraw.
    pub fn tick(&mut self) {
        if self.store.due(Instant::now())
            && let Err(e) = self.store.flush(&self.doc)
        {
            log(&format!("failed to write {}: {e}", self.path.display()));
        }
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let keys: Vec<u32> = self.outputs.keys().copied().collect();
        for key in keys {
            self.draw_output(&key);
        }
    }

    /// Read output info: name, scale and logical size.
    fn refresh_output(&mut self, output: &wl_output::WlOutput) {
        let key = key_of(output);
        let info = self.output_state.info(output);
        let name = self.name_of(output);
        if !self.outputs.contains_key(&key) {
            self.create_surface(output.clone());
        }
        self.adopt_scale(&key, output_scale(info.as_ref()));
        let Some(out) = self.outputs.get_mut(&key) else {
            return;
        };
        out.name = name;
        if let Some((w, h)) = info.and_then(|i| i.logical_size)
            && w > 0
            && h > 0
            && (out.width, out.height) != (w as u32, h as u32)
        {
            out.width = w as u32;
            out.height = h as u32;
            out.buffers.clear();
            out.last_buffer = None;
            out.last_transient = None;
            out.dirty = true;
            self.dirty = true;
        }
    }

    /// Adopt an output's scale: tell the compositor the buffer scale and drop the buffers, which
    /// were allocated for the old one. Stored coordinates stay logical, so nothing moves.
    fn adopt_scale(&mut self, key: &u32, scale: f32) {
        let Some(out) = self.outputs.get_mut(key) else {
            return;
        };
        if out.scale == scale {
            return;
        }
        out.scale = scale;
        out.surface.set_buffer_scale(scale as i32);
        out.buffers.clear();
        out.last_buffer = None;
        out.last_transient = None;
        out.dirty = true;
        self.dirty = true;
    }

    fn draw_output(&mut self, key: &u32) {
        let id = *key;
        let (format, swap) = self.shm_format();
        let Some(out) = self.outputs.get_mut(key) else {
            return;
        };
        if !out.configured || out.width == 0 || out.height == 0 {
            return;
        }
        let (w, h, scale) = (out.width, out.height, out.scale);
        let px = ((w as f32 * scale) as u32, (h as f32 * scale) as u32);
        if out.buffer_size != px {
            out.buffers.clear();
            out.last_buffer = None;
            out.last_transient = None;
            out.buffer_size = px;
        }

        // Buffers the compositor has not released cannot be drawn into: reuse a free one,
        // preferring the one that holds the previous frame, or add one up to MAX_BUFFERS.
        let previous_is_free = out.last_buffer.is_some_and(|i| {
            out.buffers
                .get_mut(i)
                .is_some_and(|b| b.canvas(&mut self.pool).is_some())
        });
        let mut usable = if previous_is_free {
            out.last_buffer
        } else {
            out.buffers
                .iter()
                .position(|b| b.canvas(&mut self.pool).is_some())
        };
        if usable.is_none() && out.buffers.len() < MAX_BUFFERS {
            match self
                .pool
                .create_buffer(px.0 as i32, px.1 as i32, (px.0 * 4) as i32, format)
            {
                Ok((buffer, _)) => {
                    out.buffers.push(buffer);
                    usable = Some(out.buffers.len() - 1);
                }
                Err(e) => {
                    log(&format!("failed to allocate a buffer: {e}"));
                    return;
                }
            }
        }
        let Some(idx) = usable else {
            // All buffers are in flight: stay dirty and retry once the compositor releases one.
            out.dirty = true;
            self.dirty = true;
            return;
        };

        // What this frame has to touch. Painting a box is only valid on the buffer that holds
        // the previous frame: on any other buffer the pixels outside the box are older. If the
        // previous buffer is in flight we fell back to the other one, so redraw everything.
        // An open text edit also forces a whole frame: its caret is not a transient box.
        let transient = transient_bounds(&out.overlay);
        let bucket = out.bucket(id);
        let empty = OutputAnnotations::default();
        let ann = self.doc.outputs.get(&bucket).unwrap_or(&empty);
        let damage = if out.dirty || out.last_buffer != Some(idx) || out.overlay.text.is_some() {
            None
        } else if out.transient_dirty {
            let transient = match (out.last_transient, transient) {
                (Some(a), Some(b)) => a.union(b),
                (Some(a), None) => a,
                (None, Some(b)) => b,
                (None, None) => Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 0.0,
                    h: 0.0,
                },
            };
            // The box has to contain everything that will be drawn, because nothing may be
            // painted outside it: those bytes still hold the previous frame, already swapped.
            Some(damage_box(ann, &out.overlay, transient, w as f32))
        } else {
            // Nothing changed for this output; another output is why the daemon redrew.
            return;
        };
        let region = match damage {
            None => DamageRegion::All,
            Some(d) => DamageRegion::from_logical(d, scale, px.0, px.1),
        };
        out.dirty = false;
        out.transient_dirty = false;
        out.last_transient = transient;
        out.last_buffer = Some(idx);

        {
            let buffer = &mut out.buffers[idx];
            let Some(canvas) = buffer.canvas(&mut self.pool) else {
                return;
            };
            // Buffers are reused, so clear to transparent or the previous frame shows through.
            region.clear(canvas, px.0, px.1);
            self.renderer
                .render(canvas, px.0, px.1, scale, ann, &out.overlay, damage);
            // Only the ARGB8888 fallback is B, G, R, A; Abgr8888 is already tiny-skia's order.
            if swap {
                region.swap_rb(canvas, px.0, px.1);
            }
        }

        match region {
            DamageRegion::All => out.surface.damage_buffer(0, 0, px.0 as i32, px.1 as i32),
            DamageRegion::Rect { x0, y0, x1, y1 } => {
                out.surface
                    .damage_buffer(x0 as i32, y0 as i32, (x1 - x0) as i32, (y1 - y0) as i32)
            }
        }
        if let Err(e) = out.buffers[idx].attach_to(&out.surface) {
            log(&format!("failed to attach the buffer: {e}"));
            return;
        }
        out.surface.commit();
    }

    /// The buffer format, decided on first use: the compositor's advertised list only arrives
    /// with the first dispatch, and every buffer an output holds has to be in that format.
    fn shm_format(&mut self) -> (wl_shm::Format, bool) {
        match self.buffer_format {
            Some(f) => f,
            None => {
                let f = buffer_format(self.shm.formats());
                self.buffer_format = Some(f);
                f
            }
        }
    }

    pub(crate) fn output_of(&self, surface: &wl_surface::WlSurface) -> Option<u32> {
        self.outputs
            .iter()
            .find(|(_, o)| &o.surface == surface)
            .map(|(key, _)| *key)
    }

    fn key_of_layer(&self, layer: &LayerSurface) -> Option<u32> {
        self.outputs
            .iter()
            .find(|(_, o)| &o.layer == layer)
            .map(|(key, _)| *key)
    }
}

fn key_of(output: &wl_output::WlOutput) -> u32 {
    output.id().protocol_id()
}

/// The output's scale factor, never below 1.
fn output_scale(info: Option<&OutputInfo>) -> f32 {
    info.map(|i| i.scale_factor.max(1) as f32).unwrap_or(1.0)
}

/// The word the edit-mode hint shows for the active tool.
fn tool_label(tool: Tool) -> &'static str {
    match tool {
        Tool::Pen => "pen",
        Tool::Eraser => "eraser",
        Tool::Text => "text",
    }
}

fn apply_input_region(compositor: &CompositorState, surface: &wl_surface::WlSurface, mode: Mode) {
    match mode {
        Mode::Locked => match Region::new(compositor) {
            // An empty region means full click-through.
            Ok(region) => surface.set_input_region(Some(region.wl_region())),
            Err(e) => log(&format!("failed to create an empty input region: {e}")),
        },
        Mode::Edit => surface.set_input_region(None),
    }
}

pub fn log(msg: &str) {
    eprintln!("[sgraffito] {msg}");
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        // Buffer pixels are logical x scale now, so the surface has to be told; the buffers
        // allocated for the old scale are dropped. Every stored coordinate stays logical.
        if let Some(key) = self.output_of(surface) {
            self.adopt_scale(&key, new_factor.max(1) as f32);
        }
        self.mark_all_dirty();
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

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if !self.outputs.contains_key(&key_of(&output)) {
            self.create_surface(output.clone());
        }
        // A single wl_output burst may only trigger new_output, so read the info here too.
        self.refresh_output(&output);
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.refresh_output(&output);
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if let Some(out) = self.outputs.remove(&key_of(&output)) {
            log(&format!("output {} removed, annotations kept", out.name));
        }
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, layer: &LayerSurface) {
        if let Some(key) = self.key_of_layer(layer)
            && let Some(old) = self.outputs.remove(&key)
        {
            log(&format!(
                "compositor closed the layer surface of {}, recreating",
                old.name
            ));
            self.create_surface(old.output.clone());
        }
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let Some(key) = self.key_of_layer(layer) else {
            return;
        };
        let Some(out) = self.outputs.get_mut(&key) else {
            return;
        };
        let (w, h) = configure.new_size;
        if w > 0 && h > 0 && (out.width, out.height) != (w, h) {
            out.width = w;
            out.height = h;
            out.buffers.clear();
            out.last_buffer = None;
            out.last_transient = None;
        }
        out.configured = true;
        out.dirty = true;
        self.dirty = true;
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            let _ = self.seat_state.get_pointer(qh, &seat);
        }
        if capability == Capability::Keyboard {
            let _ = self.seat_state.get_keyboard(qh, &seat, None);
            self.ensure_text_input(&seat);
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        _: Capability,
    ) {
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_registry!(App);

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

delegate_dispatch2!(App);
