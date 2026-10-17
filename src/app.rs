//! Wayland glue: layer surface lifecycle, mode switching, frame submission.

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use calloop::ping::Ping;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    data_device_manager::{
        DataDeviceManagerState,
        data_device::{DataDevice, DataDeviceHandler},
        data_offer::{DataOfferHandler, DragOffer, SelectionOffer},
    },
    delegate_dispatch2, delegate_registry,
    globals::GlobalData,
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
    protocol::{
        wl_data_device::WlDataDevice, wl_data_device_manager::DndAction, wl_output, wl_seat,
        wl_shm, wl_surface,
    },
};

use crate::canvas::{
    Doc, ERASER_THRESHOLD, Gesture, Grab, Handle, ItemId, Note, OutputAnnotations, Overlay, Press,
    Rect, Selection, Shape, ShapeKind, TEXT_SIZE_MAX, TEXT_SIZE_MIN, TextBuffer, TextItem,
    TextOverlay, Toolbar, Transform, View, date_stamp, marquee_select, note_drag, selection_bounds,
    selection_frame, selection_press, snap_square,
};

pub use crate::canvas::{PALETTE, Tool, ToolbarAction};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape as CursorShape, WpCursorShapeDeviceV1,
};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1;
use wayland_protocols::wp::pointer_gestures::zv1::client::zwp_pointer_gesture_pinch_v1::ZwpPointerGesturePinchV1;
use wayland_protocols::wp::pointer_gestures::zv1::client::zwp_pointer_gestures_v1::ZwpPointerGesturesV1;
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::ZwpTextInputManagerV3;
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::ZwpTextInputV3;

use crate::input::{PinchData, stepped_size};
use crate::render::{
    DamageRegion, FrameCache, Renderer, Screen, buffer_format, toolbar_bounds, transient_bounds,
    transient_damage,
};
use crate::store::{self, Store};

/// Initial stroke width and the range `[`/`]` may adjust it within, in logical pixels.
pub const PEN_WIDTH: f32 = 3.0;
pub const PEN_WIDTH_MIN: f32 = 1.0;
pub const PEN_WIDTH_MAX: f32 = 16.0;
pub const PEN_WIDTH_STEP: f32 = 1.0;
/// Initial text size and the range `[`/`]` may adjust it within, in logical pixels.
pub const TEXT_SIZE: f32 = 18.0;
pub const TEXT_SIZE_STEP: f32 = 2.0;

pub(crate) const BTN_LEFT: u32 = 0x110;
/// The wheel, pressed: a pan anchored at the point the press started on.
pub(crate) const BTN_MIDDLE: u32 = 0x112;
/// Max shm buffers kept per output: the compositor may still hold the previous one.
const MAX_BUFFERS: usize = 2;
/// How long the pop-out of a hovered block takes, one frame of it, and the relaxed poll the
/// event loop uses when nothing is moving.
const TOOLBAR_ANIM: Duration = Duration::from_millis(140);
const FRAME_WAIT: Duration = Duration::from_millis(16);
pub const IDLE_WAIT: Duration = Duration::from_millis(200);

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
    /// Only the toolbar moved (the pointer crossed onto another block), so the next frame can
    /// damage just the rail's box.
    pub(crate) toolbar_dirty: bool,
    /// Transient overlay box as drawn in the previous frame; it has to be erased next time.
    last_transient: Option<Rect>,
    /// A document change waiting to be repainted. Its box is what the next frame damages: the
    /// rest of the surface still holds the right pixels, which came from the document raster.
    pending: Option<Rect>,
    /// A committed text edit can be localized only after its previous layout was measured.
    pub(crate) text_dirty: bool,
    last_text_bounds: Option<Rect>,
    /// Buffer index that holds the previous frame. A bounding-box damage is only valid on it.
    last_buffer: Option<usize>,
    buffers: Vec<Buffer>,
    buffer_size: (u32, u32),
    pub(crate) overlay: Overlay,
    /// How this output is looking at the canvas. Never persisted, and carried across a surface
    /// recreation like `name`: losing the view on every `Esc` would be felt immediately.
    pub(crate) view: View,
    cache: FrameCache,
}

impl Output {
    /// Key used for annotations; falls back to `output-<id>` until the name arrives.
    pub(crate) fn bucket(&self, id: u32) -> Cow<'_, str> {
        if self.name.is_empty() {
            Cow::Owned(format!("output-{id}"))
        } else {
            Cow::Borrowed(&self.name)
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

    // clipboard (wl_data_device)
    data_device_manager: DataDeviceManagerState,
    data_device: Option<DataDevice>,
    /// The compositor's current selection, replaced on every selection event.
    pub(crate) selection: Option<SelectionOffer>,
    /// Pasted text, read off the event loop by a thread and drained by `tick`.
    pub(crate) paste_tx: mpsc::Sender<String>,
    pub(crate) pasted: mpsc::Receiver<String>,
    pub(crate) ping: Ping,
    /// Whether `Ctrl` is held, which is what makes `V` a paste.
    pub(crate) ctrl: bool,

    // pointer shape (wp_cursor_shape_v1)
    cursor_shape_manager: Option<WpCursorShapeManagerV1>,
    /// The seat's pointer shape device; it outlives the layer surfaces.
    pub(crate) cursor_device: Option<WpCursorShapeDeviceV1>,
    /// Serial of the latest `wl_pointer.enter`; `set_shape` is ignored without it.
    pub(crate) cursor_serial: Option<u32>,
    pub(crate) pointer_over_toolbar: bool,
    /// The block of the edit-mode toolbar the pointer is on, if any: it stretches to say what it
    /// does.
    pub(crate) toolbar_hover: Option<ToolbarAction>,
    /// When that stretch started, while it is still opening. `None` when no block is under the
    /// pointer, which is also what makes the collapse immediate.
    pub(crate) toolbar_hover_since: Option<Instant>,

    // selection (the selection tool)
    /// The gesture the pointer is dragging, if any. Nothing about a gesture exists until it is
    /// released, so none of it is in the document.
    pub(crate) gesture: Option<Gesture>,
    /// The frame handle under the pointer, which is what the pointer shape says it would grab.
    pub(crate) selection_grab: Option<Grab>,
    /// The canvas point a new note is being dragged from, while its size is being decided. The note
    /// itself lives in the overlay, so this is the one thing the drag needs that the note cannot hold.
    pub(crate) note_origin: Option<[f32; 2]>,
    /// Whether the note being dragged out has been sized by its drag, which is what tells it from a
    /// click when the pointer is released.
    pub(crate) note_sized: bool,
    /// What `Shift` and `Alt` are doing, tracked from the keyboard: a pointer event carries no modifier
    /// state of its own on Wayland, so a drag has to read it here.
    pub(crate) shift: bool,
    pub(crate) alt: bool,
    /// The copy the gesture in flight made, while it is still part of that gesture.
    pending_duplicate: Option<Duplicate>,
    /// Whether the drawer of rarely used controls is open. Transient, like the selection.
    pub(crate) more: bool,
    /// Whether edit mode paints the backdrop. A preference about reading, so it is not persisted and a
    /// new session starts with it on.
    pub(crate) scrim: bool,
    /// The pan gesture in flight: where the pointer was, so a motion event pans by its own delta. A pan
    /// is not a document change, so nothing about it is in the document, in the file, or something
    /// `Esc` has to commit or abandon.
    pub(crate) pan: Option<[f32; 2]>,

    // pointer gestures (zwp_pointer_gestures_v1; sctk does not wrap it)
    /// The manager, when the compositor has one.
    gestures: Option<ZwpPointerGesturesV1>,
    /// The pinch gesture of this seat's pointer.
    pinch: Option<ZwpPointerGesturePinchV1>,
    /// Where the pointer last was, and on which output: a pinch's focal point arrives as a delta from
    /// where the fingers went down, so the gesture needs somewhere to measure it from.
    pub(crate) pointer_at: Option<(u32, [f32; 2])>,
    /// What a pinch remembers from its `begin`: the output it is on, the screen point the fingers
    /// started at, the canvas point that was under it, and the scale the protocol last reported.
    pub(crate) pinch_state: Option<PinchState>,
}

/// A pinch in flight, as the protocol reports it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PinchState {
    pub(crate) key: u32,
    /// The screen point the fingers went down on.
    pub(crate) origin: [f32; 2],
    /// The canvas point that was under `origin`, which has to stay under the fingers.
    pub(crate) canvas: [f32; 2],
    /// The cumulative scale of the last update: the protocol reports scale relative to the start, so
    /// consecutive updates need a ratio rather than a product.
    pub(crate) scale: f32,
}

/// The copy an `Alt` drag made: what was added to the document and what it was copied from, so `Esc` can
/// put the document and the selection back where the press found them.
struct Duplicate {
    key: u32,
    copies: Vec<ItemId>,
    originals: Vec<ItemId>,
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
        data_device_manager: DataDeviceManagerState,
        ping: Ping,
    ) -> anyhow::Result<Self> {
        let path = store::path().ok_or_else(|| {
            anyhow::anyhow!("need $XDG_DATA_HOME or $HOME to locate the annotation file")
        })?;
        let doc = store::load(&path);
        let pool = SlotPool::new(1 << 20, &shm)?;
        let (paste_tx, pasted) = mpsc::channel();
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
            data_device_manager,
            data_device: None,
            selection: None,
            paste_tx,
            pasted,
            ping,
            ctrl: false,
            cursor_shape_manager: None,
            cursor_device: None,
            cursor_serial: None,
            pointer_over_toolbar: false,
            toolbar_hover: None,
            toolbar_hover_since: None,
            gesture: None,
            selection_grab: None,
            note_origin: None,
            note_sized: false,
            shift: false,
            alt: false,
            pending_duplicate: None,
            more: false,
            scrim: true,
            pan: None,
            gestures: None,
            pinch: None,
            pointer_at: None,
            pinch_state: None,
        };
        app.bind_text_input_manager();
        app.bind_cursor_shape();
        app.bind_pointer_gestures();
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
        let toolbar = self.current_toolbar();
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
                toolbar_dirty: false,
                last_transient: None,
                text_dirty: false,
                last_text_bounds: None,
                last_buffer: None,
                pending: None,
                buffers: Vec::new(),
                buffer_size: (0, 0),
                overlay: Overlay {
                    toolbar,
                    ..Default::default()
                },
                view: View::default(),
                cache: FrameCache::default(),
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
        self.end_text_edit();
        self.commit_any_gesture();
        self.cancel_transients();
        // The view is kept across a mode switch, but a gesture that was holding the pointer is not:
        // the surfaces it was panning are about to be recreated.
        self.pan = None;
        self.pinch_state = None;
        self.keyboard_focus = None;
        self.pointer_over_toolbar = false;
        self.toolbar_hover = None;
        self.toolbar_hover_since = None;
        self.sync_text_input();
        self.mode = mode;
        let keys: Vec<u32> = self.outputs.keys().copied().collect();
        for key in keys {
            let Some(old) = self.outputs.remove(&key) else {
                continue;
            };
            let (was_configured, name, view, output) = (
                old.configured,
                old.name.clone(),
                old.view,
                old.output.clone(),
            );
            drop(old);
            self.create_surface(output);
            if let Some(out) = self.outputs.get_mut(&key) {
                out.configured = was_configured;
                out.name = name;
                // The view is where the user was looking, and recreating a surface is not a reason to
                // send them back to the origin.
                out.view = view;
                out.dirty = true;
            }
        }
        log(&format!("mode switched to {mode:?}"));
        self.refresh_toolbar();
        // The shape device belongs to the seat, so it survives the recreated surfaces: re-apply
        // the shape instead of losing it with the surface that was destroyed.
        self.sync_cursor();
    }

    /// The edit-mode toolbar of the current mode, tool, colour and size.
    fn current_toolbar(&self) -> Option<Toolbar> {
        match self.mode {
            Mode::Locked => None,
            Mode::Edit => Some(Toolbar {
                tool: self.tool,
                color: PALETTE[self.color_idx],
                size: self.active_size(),
                hover: self.toolbar_hover,
                grow: self.toolbar_grow(Instant::now()),
                more: self.more,
                scrim: self.scrim,
            }),
        }
    }

    /// The size the active tool creates content with. The selection tool creates nothing and a
    /// shape is stroked with the pen, so both show the pen width, which is the size the next stroke
    /// or shape will be drawn with.
    fn active_size(&self) -> f32 {
        if self.tool.uses_pen_width() {
            self.pen_width
        } else {
            self.text_size
        }
    }

    /// `[` and `]` step the active tool's size, clamped to its range. The toolbar shows the
    /// value, so it is repainted (a whole-surface frame, like a tool or colour change).
    pub(crate) fn nudge_size(&mut self, dir: f32) {
        if self.tool.uses_pen_width() {
            self.pen_width = stepped_size(
                self.pen_width,
                dir,
                PEN_WIDTH_STEP,
                PEN_WIDTH_MIN,
                PEN_WIDTH_MAX,
            );
        } else {
            self.text_size = stepped_size(
                self.text_size,
                dir,
                TEXT_SIZE_STEP,
                TEXT_SIZE_MIN,
                TEXT_SIZE_MAX,
            );
        }
        self.refresh_toolbar();
    }

    /// Open or close the drawer. It changes which controls exist, not the rail's height, so the rail's
    /// own box is what the next frame damages.
    pub(crate) fn toggle_more(&mut self) {
        self.more = !self.more;
        // A block that no longer exists cannot still own the pointer.
        self.toolbar_hover = None;
        self.toolbar_hover_since = None;
        self.refresh_toolbar();
    }

    /// Switch the edit-mode backdrop. It is a preference about reading, not content: the document and the
    /// file are untouched, and every output's raster is dropped because the backdrop is what the raster
    /// is painted on — a frame must never carry the state that was switched away from.
    pub(crate) fn toggle_scrim(&mut self) {
        self.scrim = !self.scrim;
        self.invalidate_text_caches();
        self.mark_all_dirty();
    }

    /// Push the current toolbar into every output's transient overlay. The rail lives in the
    /// left strip and the document does not change with it, so its box is the only thing the
    /// next frame has to damage; the rest of the surface comes back from the document raster.
    fn refresh_toolbar(&mut self) {
        let toolbar = self.current_toolbar();
        for out in self.outputs.values_mut() {
            out.overlay.toolbar = toolbar;
            out.toolbar_dirty = true;
        }
        self.dirty = true;
    }

    /// The pointer crossed onto another block, or the pop-out moved on: the rail stretches a
    /// different block, or the same one a little further. Only the rail's own box changed, and it
    /// is fixed while editing, so the frame can damage just that box instead of the whole
    /// surface.
    pub(crate) fn refresh_toolbar_hover(&mut self) {
        let toolbar = self.current_toolbar();
        for out in self.outputs.values_mut() {
            out.overlay.toolbar = toolbar;
            out.toolbar_dirty = true;
        }
        self.dirty = true;
    }

    /// How far the pop-out of the block under the pointer has opened, 0 to 1.
    fn toolbar_grow(&self, now: Instant) -> f32 {
        hover_grow(self.toolbar_hover_since, now)
    }

    /// Whether a pop-out is still opening, and so whether the loop has to come back for the next
    /// frame of it.
    fn animating(&self) -> bool {
        self.toolbar_grow(Instant::now()) < 1.0
    }

    /// The toolbar control under a pointer position on an output, if any.
    pub(crate) fn toolbar_hit(&mut self, key: &u32, x: f32, y: f32) -> Option<ToolbarAction> {
        let out = self.outputs.get(key)?;
        let (toolbar, width, height) = (out.overlay.toolbar?, out.width as f32, out.height as f32);
        self.renderer.toolbar_hit(&toolbar, width, height, x, y)
    }

    /// The shape the pointer should have now: the active tool's, or the compositor's own
    /// default while the pointer is on the toolbar or the daemon is locked.
    fn desired_cursor(&self) -> CursorShape {
        if self.mode == Mode::Locked || self.pointer_over_toolbar {
            return CursorShape::Default;
        }
        if self.pan.is_some() {
            return CursorShape::Grabbing;
        }
        if self.tool == Tool::Hand {
            return CursorShape::Grab;
        }
        if self.tool == Tool::Selection {
            // With the selection tool armed the shape says what a press would grab, which is what
            // the pointer is over rather than what the tool is.
            return match (self.gesture.is_some(), self.selection_grab) {
                (true, _) => CursorShape::Grabbing,
                (false, Some(Grab::Handle(handle))) => handle_cursor(handle),
                (false, Some(Grab::Rotate)) => CursorShape::Grab,
                (false, None) => CursorShape::Default,
            };
        }
        tool_cursor(self.tool)
    }

    /// Apply the current pointer shape, when the compositor offers the protocol and the pointer
    /// has entered one of our surfaces. The request carries the latest enter serial, which the
    /// compositor matches against the one it sent, so a tool change while the pointer is
    /// already inside has to reuse it.
    pub(crate) fn sync_cursor(&mut self) {
        let Some((device, serial)) = self.cursor_device.as_ref().zip(self.cursor_serial) else {
            return;
        };
        device.set_shape(serial, self.desired_cursor());
    }

    /// Bind `zwp_pointer_gestures_v1`; without it a touchpad's two-finger zoom is simply never sent,
    /// and everything else about panning and zooming still works.
    pub(crate) fn bind_pointer_gestures(&mut self) {
        let bound = self
            .registry_state
            .bind_one::<ZwpPointerGesturesV1, App, PinchData>(&self.qh, 1..=3, PinchData);
        match bound {
            Ok(manager) => {
                self.gestures = Some(manager);
                log("pointer gestures protocol zwp_pointer_gestures_v1 available");
            }
            Err(e) => log(&format!(
                "compositor does not provide zwp_pointer_gestures_v1 ({e}); a touchpad's pinch cannot zoom, so use the wheel or the hand tool"
            )),
        }
    }

    /// Bind `wp_cursor_shape_v1`; the pointer keeps the compositor's own shape without it.
    fn bind_cursor_shape(&mut self) {
        let bound = self
            .registry_state
            .bind_one::<WpCursorShapeManagerV1, App, GlobalData>(&self.qh, 1..=2, GlobalData);
        match bound {
            Ok(manager) => {
                self.cursor_shape_manager = Some(manager);
                log("pointer shape protocol wp_cursor_shape_v1 available");
            }
            Err(e) => log(&format!(
                "compositor does not provide wp_cursor_shape_v1 ({e}); the pointer keeps the compositor's own shape"
            )),
        }
    }

    pub(crate) fn set_tool(&mut self, tool: Tool) {
        if self.tool != tool {
            self.end_text_edit();
            self.commit_transient_drawing();
            // A gesture in progress is written rather than dropped, and then the selection goes
            // with the tool that could act on it.
            self.commit_any_gesture();
            self.drop_selection();
            self.tool = tool;
            log(&format!("tool switched to {tool:?}"));
            // The document is untouched: the frame repaints the rail and whatever the finished
            // edit or stroke left behind, not the surface.
            self.refresh_toolbar();
            self.sync_cursor();
        }
    }

    fn mark_all_dirty(&mut self) {
        self.dirty = true;
        for out in self.outputs.values_mut() {
            out.dirty = true;
        }
    }

    pub(crate) fn invalidate_text_caches(&mut self) {
        for out in self.outputs.values_mut() {
            out.cache.invalidate();
        }
    }

    /// The document changed: schedule the debounced write and drop every output's raster, so the
    /// next frame of each one is rebuilt from the new document. Every mutation of the document
    /// goes through here, which is what keeps a stale raster from surviving a change.
    pub(crate) fn doc_changed(&mut self) {
        self.store.mark_dirty(Instant::now());
        self.invalidate_text_caches();
        self.dirty = true;
    }

    /// A document change whose box is known: the frame that follows damages that box instead of
    /// the whole surface, because nothing outside it changed.
    pub(crate) fn doc_changed_at(&mut self, key: u32, box_: Rect) {
        self.doc_changed();
        if let Some(out) = self.outputs.get_mut(&key) {
            out.pending = Some(match out.pending {
                Some(pending) => pending.union(box_),
                None => box_,
            });
        }
    }

    /// Commit any in-progress stroke or shape before a tool change or a new pointer gesture.
    pub(crate) fn commit_transient_drawing(&mut self) {
        let mut pending = Vec::new();
        let mut shapes = Vec::new();
        let mut overlay_changed = false;
        for (key, out) in self.outputs.iter_mut() {
            if let Some(stroke) = out.overlay.stroke.take() {
                if !stroke.points.is_empty() {
                    pending.push((*key, out.bucket(*key).into_owned(), stroke));
                }
                overlay_changed = true;
            }
            if let Some(shape) = out.overlay.shape.take() {
                if !shape.is_empty() {
                    shapes.push((*key, out.bucket(*key).into_owned(), shape));
                }
                overlay_changed = true;
            }
            if out.overlay.eraser.take().is_some() {
                overlay_changed = true;
            }
        }
        // The first output to commit reports the change; every output is drawn every iteration
        // anyway, and their rasters were all dropped by `doc_changed_at`.
        let key = pending
            .first()
            .map(|(key, _, _)| *key)
            .or_else(|| shapes.first().map(|(key, _, _)| *key))
            .unwrap_or_default();
        let mut committed_box: Option<Rect> = None;
        let committed = !pending.is_empty() || !shapes.is_empty();
        for (_, bucket, stroke) in pending {
            if let Some(box_) = stroke.bounds() {
                committed_box = Some(match committed_box {
                    Some(pending) => pending.union(box_),
                    None => box_,
                });
            }
            self.doc
                .outputs
                .entry(bucket)
                .or_default()
                .strokes
                .push(stroke);
        }
        for (_, bucket, shape) in shapes {
            let box_ = shape.bounds();
            committed_box = Some(match committed_box {
                Some(pending) => pending.union(box_),
                None => box_,
            });
            self.doc
                .outputs
                .entry(bucket)
                .or_default()
                .shapes
                .push(shape);
        }
        if committed {
            self.doc_changed_at(key, committed_box.unwrap_or_default());
        }
        // A committed stroke is repainted from its own box; anything else here only lost a
        // transient element, which is a whole-surface frame as before.
        if overlay_changed && !committed {
            self.mark_all_dirty();
        }
    }

    /// Drop transient drawing state when its surface or the current operation disappears.
    pub(crate) fn cancel_transient(&mut self, key: u32) {
        self.note_origin = None;
        self.note_sized = false;
        let Some(out) = self.outputs.get_mut(&key) else {
            return;
        };
        let changed = out.overlay.stroke.take().is_some()
            || out.overlay.shape.take().is_some()
            || out.overlay.eraser.take().is_some()
            || out.overlay.note.take().is_some()
            || out.overlay.selection.take().is_some()
            || out.overlay.marquee.take().is_some();
        if changed {
            out.transient_dirty = true;
            out.dirty = true;
            self.dirty = true;
        }
    }

    /// With the selection tool armed, a press grabs a handle of the frame, takes the element under
    /// the pointer, or starts a marquee. A handle wins over the element below it, so a resize that
    /// starts on a corner never moves the selection instead.
    pub(crate) fn select_press(&mut self, key: u32, x: f32, y: f32) {
        // The decision is `canvas::selection_press`, where the two coordinate spaces — the screen the
        // handles live in and the canvas a gesture moves the document in — are one function a test can
        // reach. This only carries it out.
        let (press, frame, at) = {
            let Some(out) = self.outputs.get(&key) else {
                return;
            };
            let view = out.view;
            let bucket = out.bucket(key).into_owned();
            let empty = OutputAnnotations::default();
            let ann = self.doc.outputs.get(&bucket).unwrap_or(&empty);
            let held = out
                .overlay
                .selection
                .as_ref()
                .map(|s| s.items.clone())
                .unwrap_or_default();
            let press =
                selection_press(ann, &held, view, [x, y], view.canvas_len(ERASER_THRESHOLD));
            let frame = selection_frame(ann, &held, None);
            (press, frame, view.to_canvas([x, y]))
        };
        let alt = self.alt;
        match press {
            // A gesture works in the document's own coordinates, because that is what the transform it
            // produces is applied to.
            Press::Handle(grab) => {
                self.gesture = Some(Gesture::start(
                    Some(grab),
                    frame.unwrap_or_default(),
                    at[0],
                    at[1],
                    alt,
                ));
                self.sync_cursor();
            }
            Press::Move => {
                self.gesture = Some(Gesture::start(
                    None,
                    frame.unwrap_or_default(),
                    at[0],
                    at[1],
                    alt,
                ));
            }
            Press::Element(id) => {
                self.set_selection(key, vec![id], None);
                self.gesture = Some(Gesture::start(None, Rect::default(), at[0], at[1], alt));
            }
            Press::Marquee { canvas } => {
                // The marquee is a canvas rectangle: the frame it is drawn in and the elements it
                // catches are both canvas, and only the pointer has to be converted once, here.
                self.gesture = Some(Gesture::Marquee { origin: canvas });
            }
        }
    }

    /// Drag the live gesture to the pointer: a marquee sweeps its rectangle, the other three move
    /// the held geometry. An identity transform is stored as no transform at all, so a press that
    /// never moved leaves the document raster alone.
    pub(crate) fn drag_selection(&mut self, key: u32, x: f32, y: f32) {
        let Some(gesture) = self.gesture else {
            return;
        };
        // The pointer reports screen coordinates and a gesture moves the document, so the pointer is
        // converted once here: without it the marquee and the drag drift away from the cursor as soon
        // as the view is panned or zoomed.
        let Some(at) = self.outputs.get(&key).map(|out| out.view.to_canvas([x, y])) else {
            return;
        };
        if let Gesture::Marquee { origin } = gesture {
            let marquee = Rect::from_corners(origin, at);
            let Some(out) = self.outputs.get_mut(&key) else {
                return;
            };
            if out.overlay.marquee != Some(marquee) {
                out.overlay.marquee = Some(marquee);
                out.transient_dirty = true;
                self.dirty = true;
            }
            return;
        }
        let Some(transform) = gesture.transform(at, self.shift) else {
            return;
        };
        // An `Alt` drag copies what it holds and then drags the copy, leaving the original where it was.
        // The copy is made at the first movement, because a press that never moves still writes nothing,
        // and the first movement is where the transform stops being the identity.
        if matches!(
            gesture,
            Gesture::Move {
                duplicate: true,
                ..
            }
        ) && self.pending_duplicate.is_none()
            && !transform.is_identity()
        {
            self.duplicate_selection(key);
        }
        let next = (!transform.is_identity()).then_some(transform);
        let Some(out) = self.outputs.get_mut(&key) else {
            return;
        };
        let Some(selection) = out.overlay.selection.as_mut() else {
            return;
        };
        if selection.transform != next {
            selection.transform = next;
            out.transient_dirty = true;
            self.dirty = true;
        }
    }

    /// Copy what the selection holds and hand the selection to the copies, so the gesture that is running
    /// drags the copy while the original stays where it was. The copy is recorded until the gesture is
    /// over, because `Esc` has to be able to take it back.
    fn duplicate_selection(&mut self, key: u32) {
        let Some(out) = self.outputs.get(&key) else {
            return;
        };
        let Some(selection) = out.overlay.selection.as_ref() else {
            return;
        };
        let originals = selection.items.clone();
        let transform = selection.transform;
        let bucket = out.bucket(key).into_owned();
        let Some((copies, box_)) = self
            .doc
            .outputs
            .get_mut(&bucket)
            .map(|ann| ann.duplicate(&originals))
        else {
            return;
        };
        if copies.is_empty() {
            return;
        }
        self.pending_duplicate = Some(Duplicate {
            key,
            copies: copies.clone(),
            originals,
        });
        // The gesture names the copies from here on, so the transform the drag is carrying lands on them.
        self.set_selection(key, copies, transform);
        if let Some(box_) = box_ {
            self.doc_changed_at(key, box_);
        }
    }

    /// Take the copy of the gesture in flight back out of the document and give the selection back to the
    /// elements it was made from: `Esc` on an `Alt` drag leaves the press's world exactly as it was.
    pub(crate) fn drop_duplicate(&mut self) -> bool {
        let Some(dup) = self.pending_duplicate.take() else {
            return false;
        };
        let Some(out) = self.outputs.get(&dup.key) else {
            return false;
        };
        let bucket = out.bucket(dup.key).into_owned();
        let removed = self
            .doc
            .outputs
            .get_mut(&bucket)
            .and_then(|ann| ann.remove(&dup.copies));
        self.set_selection(dup.key, dup.originals, None);
        if let Some(box_) = removed {
            self.doc_changed_at(dup.key, box_);
        }
        true
    }

    /// Let go of the pointer: a marquee becomes the selection, and a dragged gesture is written
    /// into the document.
    pub(crate) fn release_selection(&mut self, key: u32) {
        let Some(gesture) = self.gesture.take() else {
            return;
        };
        if matches!(gesture, Gesture::Marquee { .. }) {
            self.finish_marquee(key);
        } else {
            self.write_gesture(key);
        }
        // The drag is over, so the pointer stops saying it is holding something. The next motion
        // settles it on whatever is under it now.
        self.selection_grab = None;
        self.sync_cursor();
    }

    /// Start panning from a screen point. A pan takes the pointer for its duration: it cannot also
    /// draw, erase, select or place text, and a gesture in flight is finished rather than continued
    /// beside it.
    pub(crate) fn begin_pan(&mut self, key: u32, x: f32, y: f32) {
        self.end_text_edit();
        self.commit_transient_drawing();
        self.commit_any_gesture();
        self.pan = Some([x, y]);
        // Nothing else owns the pointer now.
        self.gesture = None;
        self.selection_grab = None;
        let _ = key;
        self.sync_cursor();
    }

    /// Pan the output the pointer is on by the pointer's own movement, so the canvas point under it
    /// stays under it.
    pub(crate) fn drag_pan(&mut self, key: u32, x: f32, y: f32) {
        let Some(last) = self.pan.as_mut() else {
            return;
        };
        let delta = [x - last[0], y - last[1]];
        *last = [x, y];
        if delta == [0.0, 0.0] {
            return;
        }
        self.view_changed(key, |view| view.pan_by(delta));
    }

    /// Zoom about a screen point, which stays where it is.
    pub(crate) fn zoom_about(&mut self, key: u32, anchor: [f32; 2], factor: f32) {
        self.view_changed(key, |view| view.zoom_about(anchor, factor));
    }

    /// Let go of a pan.
    pub(crate) fn end_pan(&mut self) {
        if self.pan.take().is_some() {
            self.sync_cursor();
        }
    }

    /// A pinch started: remember where the fingers are and what is under them, because the protocol
    /// reports everything about a pinch as a delta from here.
    pub(crate) fn pinch_begin(
        &mut self,
        surface: &wayland_client::protocol::wl_surface::WlSurface,
    ) {
        let Some(key) = self.output_of(surface) else {
            return;
        };
        let Some((at_key, at)) = self.pointer_at else {
            return;
        };
        if at_key != key {
            return;
        }
        let view = match self.outputs.get(&key) {
            Some(out) => out.view,
            None => return,
        };
        // A pinch is a view gesture, so it takes the pointer like a pan does.
        self.pan = None;
        self.gesture = None;
        self.pinch_state = Some(PinchState {
            key,
            origin: at,
            canvas: view.to_canvas(at),
            scale: 1.0,
        });
        self.sync_cursor();
    }

    /// One pinch update: the focal point has moved by `(dx, dy)` from where the fingers went down and
    /// the scale is cumulative, so both are relative to what `pinch_begin` remembered.
    pub(crate) fn pinch_update(&mut self, dx: f32, dy: f32, scale: f32) {
        let Some(state) = self.pinch_state.as_mut() else {
            return;
        };
        let factor = if state.scale > 0.0 {
            scale / state.scale
        } else {
            1.0
        };
        state.scale = scale;
        let anchor = [state.origin[0] + dx, state.origin[1] + dy];
        let (key, canvas) = (state.key, state.canvas);
        if factor != 1.0 {
            self.view_changed(key, |view| view.zoom_onto(factor, canvas, anchor));
        } else {
            // The fingers only moved: the canvas point they started on follows them.
            self.view_changed(key, |view| {
                let zoom = view.zoom;
                view.offset = [anchor[0] - canvas[0] * zoom, anchor[1] - canvas[1] * zoom];
            });
        }
    }

    /// A pinch ended.
    pub(crate) fn pinch_end(&mut self) {
        if self.pinch_state.take().is_some() {
            self.sync_cursor();
        }
    }

    /// Whether a pan is in flight, which is what the pointer shape and the press path both ask.
    pub(crate) fn panning(&self) -> bool {
        self.pan.is_some()
    }

    /// Apply a change to one output's view. The raster is screen-space, so a view change moves every
    /// pixel of it: the cache is dropped, the boxes the previous frame cached describe a view that no
    /// longer exists, and the frame that follows repaints the whole surface.
    pub(crate) fn view_changed(&mut self, key: u32, f: impl FnOnce(&mut View)) {
        let Some(out) = self.outputs.get_mut(&key) else {
            return;
        };
        f(&mut out.view);
        out.pending = None;
        out.last_transient = None;
        out.last_text_bounds = None;
        out.cache.invalidate();
        out.dirty = true;
        self.dirty = true;
    }

    /// Drop the selection everywhere, and the marquee with it. Returns whether anything was held.
    pub(crate) fn drop_selection(&mut self) -> bool {
        // The record names indices, and a selection that has gone is not a gesture that can still be
        // taken back: keeping it would let a later `Esc` remove whatever the indices point at then.
        self.pending_duplicate = None;
        let mut dropped = false;
        for out in self.outputs.values_mut() {
            let had = out.overlay.selection.take().is_some() | out.overlay.marquee.take().is_some();
            if had {
                out.transient_dirty = true;
                dropped = true;
            }
        }
        if dropped {
            self.gesture = None;
            self.dirty = true;
        }
        dropped
    }

    /// Remove every element the selection holds, as one document change: the box the removed ones
    /// occupied is what the next frame damages. A gesture still in flight is abandoned rather than
    /// written — it would put a transform into the file for elements that are about to be gone — and
    /// the selection is cleared last, by the same helper the other paths use. Returns whether
    /// anything was removed.
    pub(crate) fn delete_selection(&mut self) -> bool {
        let held: Vec<(u32, String, Vec<ItemId>)> = self
            .outputs
            .iter()
            .filter_map(|(key, out)| {
                out.overlay
                    .selection
                    .as_ref()
                    .map(|s| (*key, out.bucket(*key).into_owned(), s.items.clone()))
            })
            .collect();
        let mut removed = Vec::new();
        for (key, bucket, items) in held {
            if let Some(box_) = self
                .doc
                .outputs
                .get_mut(&bucket)
                .and_then(|ann| ann.remove(&items))
            {
                removed.push((key, box_));
            }
        }
        let deleted = !removed.is_empty();
        // The gesture goes with the selection, whether or not anything was removed: its elements are
        // gone, and a drag nothing showed must not reach the file.
        self.drop_selection();
        for (key, box_) in removed {
            self.doc_changed_at(key, box_);
        }
        deleted
    }

    /// Write any live gesture into the document, wherever it is. Something other than the pointer
    /// ends the gesture this way — a mode switch, a tool change, the output going away — and the
    /// drag the user already made is kept rather than dropped.
    pub(crate) fn commit_any_gesture(&mut self) {
        let keys: Vec<u32> = self
            .outputs
            .iter()
            .filter(|(_, out)| {
                out.overlay
                    .selection
                    .as_ref()
                    .is_some_and(|s| s.transform.is_some())
            })
            .map(|(key, _)| *key)
            .collect();
        for key in keys {
            self.write_gesture(key);
        }
        self.gesture = None;
    }

    /// Hold `items` on one output and let go everywhere else: at most one selection exists at a
    /// time, the way at most one text box is under edit.
    fn set_selection(&mut self, key: u32, items: Vec<ItemId>, transform: Option<Transform>) {
        let owned =
            (!items.is_empty() || transform.is_some()).then_some(Selection { items, transform });
        for (k, out) in self.outputs.iter_mut() {
            let next = if *k == key { owned.clone() } else { None };
            if out.overlay.selection != next {
                out.overlay.selection = next;
                out.transient_dirty = true;
                self.dirty = true;
            }
        }
    }

    /// Let the marquee's rectangle decide the selection: the elements it wholly contains.
    fn finish_marquee(&mut self, key: u32) {
        let marquee = {
            let Some(out) = self.outputs.get_mut(&key) else {
                return;
            };
            out.transient_dirty = true;
            match out.overlay.marquee.take() {
                Some(marquee) => marquee,
                None => return,
            }
        };
        let bucket = self
            .outputs
            .get(&key)
            .map(|out| out.bucket(key).into_owned());
        let items = match bucket {
            Some(bucket) => {
                let empty = OutputAnnotations::default();
                let ann = self.doc.outputs.get(&bucket).unwrap_or(&empty);
                marquee_select(ann, marquee)
            }
            None => Vec::new(),
        };
        self.set_selection(key, items, None);
    }

    /// Write a dragged transform into the document, so the geometry the frame has been drawing is
    /// the geometry the file holds. An element that is no longer there is dropped instead of being
    /// written over whatever took its place, which is what stops a `clear` arriving mid-drag from
    /// resurrecting what it removed.
    fn write_gesture(&mut self, key: u32) {
        let Some(out) = self.outputs.get(&key) else {
            return;
        };
        let Some(selection) = out.overlay.selection.as_ref() else {
            return;
        };
        let Some(transform) = selection.transform else {
            return;
        };
        let (items, bucket) = (selection.items.clone(), out.bucket(key).into_owned());
        let mut box_: Option<Rect> = None;
        if let Some(ann) = self.doc.outputs.get_mut(&bucket) {
            for id in &items {
                let changed = match *id {
                    ItemId::Stroke(i) if i < ann.strokes.len() => {
                        let turned = transform.stroke(&ann.strokes[i]);
                        let changed = ann.strokes[i]
                            .bounds()
                            .into_iter()
                            .chain(turned.bounds())
                            .reduce(Rect::union);
                        ann.strokes[i] = turned;
                        changed
                    }
                    ItemId::Text(i) if i < ann.texts.len() => {
                        let moved = transform.text(&ann.texts[i]);
                        let changed = ann.texts[i].paint_bounds().union(moved.paint_bounds());
                        ann.texts[i] = moved;
                        Some(changed)
                    }
                    ItemId::Note(i) if i < ann.notes.len() => {
                        let moved = transform.note(&ann.notes[i]);
                        let changed = ann.notes[i].bounds().union(moved.bounds());
                        ann.notes[i] = moved;
                        Some(changed)
                    }
                    ItemId::Shape(i) if i < ann.shapes.len() => {
                        let moved = transform.shape(&ann.shapes[i]);
                        let changed = ann.shapes[i].bounds().union(moved.bounds());
                        ann.shapes[i] = moved;
                        Some(changed)
                    }
                    _ => None,
                };
                box_ = match (box_, changed) {
                    (Some(a), Some(b)) => Some(a.union(b)),
                    (a, b) => a.or(b),
                };
            }
        }
        // A copy the gesture made is ordinary content from here on: it has been dragged, and the gesture
        // that owned it is over.
        self.pending_duplicate = None;
        // The held elements are the document's again, so the raster has to hold them where they
        // now are: dropping the transform is what puts them back into it.
        if let Some(out) = self.outputs.get_mut(&key)
            && let Some(selection) = out.overlay.selection.as_mut()
        {
            selection.transform = None;
        }
        if let Some(box_) = box_ {
            self.doc_changed_at(key, box_);
        }
        // A resized note re-wraps, and the text wins where it needs more room than the resize left: the
        // height a gesture gives a note is a minimum, re-measured here the way typing re-measures it.
        let screen = self.outputs.get(&key).map(|out| Screen {
            scale: out.scale,
            view: out.view,
        });
        if let Some(screen) = screen {
            let notes: Vec<Note> = self
                .doc
                .outputs
                .get(&bucket)
                .map(|ann| {
                    items
                        .iter()
                        .filter_map(|id| match id {
                            ItemId::Note(i) => ann.notes.get(*i).cloned(),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            for note in notes {
                let needed = self.renderer.wrapped_text_height(&note.text, &note, screen);
                let h = note.height_for(needed);
                if h <= note.h {
                    continue;
                }
                if let Some(slot) = self
                    .doc
                    .outputs
                    .get_mut(&bucket)
                    .and_then(|ann| ann.notes.iter_mut().find(|n| n.bounds() == note.bounds()))
                {
                    slot.h = h;
                }
            }
        }
    }

    fn cancel_transients(&mut self) {
        let keys: Vec<u32> = self.outputs.keys().copied().collect();
        for key in keys {
            self.cancel_transient(key);
        }
    }

    pub(crate) fn set_color(&mut self, idx: usize) {
        if idx < PALETTE.len() {
            self.color_idx = idx;
            self.refresh_toolbar();
            self.recolour_selection(PALETTE[idx]);
        }
    }

    /// The other half of a colour chip: what the selection holds takes the colour the chip shows, as one
    /// document change reported at the union of the boxes those elements occupy. The selection is left
    /// held, so a second chip recolours the same elements, and the frame stays where it was, which is what
    /// shows the user what they just changed.
    fn recolour_selection(&mut self, color: &str) {
        let held: Vec<(u32, String, Vec<ItemId>)> = self
            .outputs
            .iter()
            .filter_map(|(key, out)| {
                out.overlay
                    .selection
                    .as_ref()
                    .map(|s| (*key, out.bucket(*key).into_owned(), s.items.clone()))
            })
            .collect();
        let mut changed = Vec::new();
        for (key, bucket, items) in held {
            if let Some(box_) = self
                .doc
                .outputs
                .get_mut(&bucket)
                .and_then(|ann| ann.recolour(&items, color))
            {
                changed.push((key, box_));
            }
        }
        for (key, box_) in changed {
            self.doc_changed_at(key, box_);
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
            out.text_dirty = false;
            // The box the edit occupied: the caret and the live text were painted there, and the
            // committed item takes their place. Nothing outside it moves, so the frame that
            // reports the commit damages it and fills the rest from the raster.
            let previous = out.last_text_bounds;
            out.last_text_bounds = None;
            // The cached box is screen-space and the item's own is canvas, so the union is taken on
            // screen: mixing them would damage the wrong region at any zoom but 1:1.
            let view = out.view;
            let (scale, bucket) = (out.scale, out.bucket(id).into_owned());
            let mut box_ = previous.unwrap_or_default();
            // A note being edited is content: its text goes back into the note, with the height the
            // wrapped text needs, and a note left empty is dropped as an empty text box is.
            if let Some(index) = edit.note {
                let note = self
                    .doc
                    .outputs
                    .get(&bucket)
                    .and_then(|ann| ann.notes.get(index))
                    .cloned();
                let Some(mut note) = note else { continue };
                box_ = box_.union(view.screen_rect(note.bounds()));
                let text = edit.buffer.text.clone();
                // An empty note stays: an empty sticky is a sticky, and it is what the user dragged
                // out. Dropping it made a note the user had just placed vanish the moment they
                // started the next one, which reads as the note having failed to be created.
                if let Some(ann) = self.doc.outputs.get_mut(&bucket) {
                    let needed =
                        self.renderer
                            .wrapped_text_height(&text, &note, Screen { scale, view });
                    note.text = text;
                    note = note.grown_for(needed);
                    box_ = box_.union(view.screen_rect(note.bounds()));
                    if index < ann.notes.len() {
                        ann.notes[index] = note;
                    }
                }
                out.pending = Some(match out.pending {
                    Some(pending) => pending.union(box_),
                    None => box_,
                });
                changed = true;
                continue;
            }
            if edit.buffer.text.is_empty() {
                out.pending = Some(match out.pending {
                    Some(pending) => pending.union(box_),
                    None => box_,
                });
                continue;
            }
            let mut item = edit.item.clone();
            item.text = edit.buffer.text.clone();
            box_ = box_.union(view.screen_rect(item.paint_bounds()));
            out.pending = Some(match out.pending {
                Some(pending) => pending.union(box_),
                None => box_,
            });
            let ann = self.doc.outputs.entry(bucket).or_default();
            match edit.index {
                Some(idx) if idx < ann.texts.len() => ann.texts[idx] = item,
                _ => ann.texts.push(item),
            }
            changed = true;
        }
        if changed {
            self.doc_changed();
        }
        self.im_preedit = None;
        self.im_commit.clear();
        self.im_delete = None;
        self.dirty = true;
        self.sync_text_input();
    }

    pub(crate) fn begin_text_edit(&mut self, key: &u32, x: f32, y: f32) {
        let id = *key;
        let Some(out) = self.outputs.get(key) else {
            return;
        };
        let bucket = out.bucket(id);
        let scale = out.scale;
        // The click is tested against the text the renderer measures, not against the font-size
        // estimate the eraser uses: that estimate is 0.6 em per character, which is too narrow
        // for CJK, so clicking such a glyph used to start a new box instead of editing it.
        let existing = self.doc.outputs.get(bucket.as_ref()).and_then(|ann| {
            ann.texts.iter().position(|t| {
                self.renderer
                    .text_edit_bounds(
                        &t.text,
                        t,
                        Screen {
                            scale,
                            view: out.view,
                        },
                    )
                    .contains(x, y)
            })
        });
        // A note is a thing you write in, so the text tool enters it rather than dropping a text box on
        // top of it.
        let on_note = self
            .doc
            .outputs
            .get(bucket.as_ref())
            .and_then(|ann| ann.notes.iter().position(|n| n.bounds().contains(x, y)));
        if let Some(index) = on_note {
            self.enter_note(key, index);
            return;
        }
        let (item, index) = match existing {
            Some(idx) => (
                self.doc.outputs[bucket.as_ref()].texts[idx].clone(),
                Some(idx),
            ),
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
        log(&format!(
            "text edit on {}",
            if index.is_some() {
                "an existing box"
            } else {
                "a new box"
            }
        ));
        if let Some(out) = self.outputs.get_mut(key) {
            out.overlay.text = Some(TextOverlay {
                buffer: TextBuffer::new(item.text.clone()),
                item,
                index,
                note: None,
                preedit: String::new(),
            });
            out.text_dirty = false;
            out.last_text_bounds = None;
            out.dirty = true;
        }
        self.dirty = true;
        self.sync_text_input();
    }

    /// Start a shape at the press point. Motion moves its second point and the release commits it,
    /// so a shape is the pen's gesture with a kind, and a live shape is transient exactly as an
    /// in-progress stroke is. A shape is stroked with the pen width and never filled.
    pub(crate) fn begin_shape(&mut self, key: &u32, x: f32, y: f32, kind: ShapeKind) {
        let color = PALETTE[self.color_idx].to_string();
        let width = self.pen_width;
        let Some(out) = self.outputs.get_mut(key) else {
            return;
        };
        out.overlay.shape = Some(Shape {
            kind,
            color,
            width,
            from: [x, y],
            to: [x, y],
        });
    }

    /// Grow the note being typed in to hold its text. Measured through the renderer, because the height
    /// a wrapped text needs is a font question; the pure layer only stores the height it is given. The
    /// overlay's item follows the note's box, so the caret and the measured bounds move with it.
    pub(crate) fn grow_note(&mut self) {
        let job = self.outputs.iter().find_map(|(key, out)| {
            let edit = out.overlay.text.as_ref()?;
            Some((
                *key,
                edit.note?,
                out.scale,
                out.view,
                edit.buffer.text.clone(),
            ))
        });
        let Some((key, index, scale, view, text)) = job else {
            return;
        };
        let Some(bucket) = self
            .outputs
            .get(&key)
            .map(|out| out.bucket(key).into_owned())
        else {
            return;
        };
        let Some(note) = self
            .doc
            .outputs
            .get(&bucket)
            .and_then(|ann| ann.notes.get(index))
            .cloned()
        else {
            return;
        };
        let needed = self
            .renderer
            .wrapped_text_height(&text, &note, Screen { scale, view });
        let updated = note.grown_for(needed);
        if let Some(ann) = self.doc.outputs.get_mut(&bucket)
            && let Some(slot) = ann.notes.get_mut(index)
        {
            *slot = updated.clone();
        }
        if let Some(out) = self.outputs.get_mut(&key)
            && let Some(edit) = out.overlay.text.as_mut()
        {
            edit.item = updated.text_item();
        }
    }

    /// Create a note at a point and put the caret in it, or enter the note that is already there: a
    /// press on an existing note must not stack another one on top of it.
    pub(crate) fn begin_note(&mut self, key: &u32, x: f32, y: f32) {
        let id = *key;
        let Some(out) = self.outputs.get(key) else {
            return;
        };
        let bucket = out.bucket(id).into_owned();
        let existing = self
            .doc
            .outputs
            .get(&bucket)
            .and_then(|ann| ann.notes.iter().position(|n| n.bounds().contains(x, y)));
        // An existing note is entered; a new one is dragged out and only becomes content on the
        // release, so that a drag decides its size the way the shape tools' drags decide theirs.
        let Some(index) = existing else {
            // Nothing is placed here: the note does not exist until the pointer has been dragged past
            // the slop, so a click cannot leave one on the screen — not even for the frame between the
            // press and the release, which is what a click that only flickered a default-sized square
            // was showing.
            self.note_origin = Some([x, y]);
            self.note_sized = false;
            return;
        };
        log("note edit on an existing note");
        self.enter_note(key, index);
    }

    /// Size the note being dragged out. A drag that never leaves the minimum is a click, which gives the
    /// default size; a real drag gives the rectangle it swept, in any direction.
    pub(crate) fn drag_note(&mut self, key: &u32, x: f32, y: f32) {
        let Some(origin) = self.note_origin else {
            return;
        };
        // `Shift` makes the note square, the way it makes a closed shape square.
        let at = if self.shift {
            snap_square(origin, [x, y])
        } else {
            [x, y]
        };
        // The rule lives in `canvas::note_drag`, where a test can reach it: a pointer that has not moved
        // is not a drag, and a drag gives the rectangle it swept.
        let Some(swept) = note_drag(origin, at) else {
            return;
        };
        let Some(out) = self.outputs.get_mut(key) else {
            return;
        };
        // The note is created by the first size the drag produces, so an aborted click never had one.
        let note = out.overlay.note.get_or_insert_with(|| {
            Note::new(
                origin[0],
                origin[1],
                PALETTE[self.color_idx],
                date_stamp(SystemTime::now()),
            )
        });
        note.x = swept.x;
        note.y = swept.y;
        note.w = swept.w;
        note.h = swept.h;
        self.note_sized = true;
        out.transient_dirty = true;
        self.dirty = true;
    }

    /// The release of a note's drag: it becomes content and the caret goes in it, which is the "waits for
    /// text" the note tool promises.
    pub(crate) fn commit_note(&mut self, key: &u32) {
        self.note_origin = None;
        let sized = std::mem::take(&mut self.note_sized);
        let Some(out) = self.outputs.get_mut(key) else {
            return;
        };
        // A click is not a note: the note tool sizes a note the way a shape tool draws a shape, so a press
        // that never dragged leaves nothing behind. There is nothing to drop either — the note was never
        // created.
        let Some(note) = out.overlay.note.take().filter(|_| sized) else {
            return;
        };
        out.transient_dirty = true;
        let bucket = out.bucket(*key).into_owned();
        let index = {
            let ann = self.doc.outputs.entry(bucket).or_default();
            ann.notes.push(note);
            ann.notes.len() - 1
        };
        self.enter_note(key, index);
    }

    /// Edit a note's text through the same overlay a text box uses, with the overlay naming the note so
    /// that the caret, the preedit, the input method and the commit all know where the text belongs.
    fn enter_note(&mut self, key: &u32, index: usize) {
        let id = *key;
        let Some(out) = self.outputs.get(key) else {
            return;
        };
        let bucket = out.bucket(id).into_owned();
        let Some(note) = self
            .doc
            .outputs
            .get(&bucket)
            .and_then(|ann| ann.notes.get(index))
            .cloned()
        else {
            return;
        };
        if let Some(out) = self.outputs.get_mut(key) {
            out.overlay.text = Some(TextOverlay {
                buffer: TextBuffer::new(note.text.clone()),
                item: note.text_item(),
                index: None,
                note: Some(index),
                preedit: String::new(),
            });
            out.text_dirty = false;
            out.last_text_bounds = None;
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
                // `clear` empties the document, so a gesture still holding elements of it is
                // dropped: writing it would put back what this command just removed.
                self.gesture = None;
                self.cancel_transients();
                self.doc.outputs.clear();
                self.invalidate_text_caches();
                self.mark_all_dirty();
                let _ = self.connection.flush();
                match self.store.flush(&self.doc) {
                    Ok(()) => "ok".into(),
                    Err(e) => format!("error: {e}"),
                }
            }
        }
    }

    /// Called once per event loop iteration: flush pending writes and redraw.
    /// Called once per event-loop iteration: flush pending writes and redraw. Returns how long
    /// the loop may wait before the next iteration, which is a frame while a pop-out is opening.
    pub fn tick(&mut self) -> Duration {
        if self.store.due(Instant::now())
            && let Err(e) = self.store.flush(&self.doc)
        {
            log(&format!("failed to write {}: {e}", self.path.display()));
        }
        // A paste arrives on a thread; the ping that woke this iteration put it here.
        self.take_pasted();
        // The rail is the only thing that animates, and a frame of it is a localized repaint.
        if self.animating() {
            self.refresh_toolbar_hover();
        }
        if self.dirty {
            self.dirty = false;
            let keys: Vec<u32> = self.outputs.keys().copied().collect();
            for key in keys {
                self.draw_output(&key);
            }
        }
        self.next_wait()
    }

    /// The wait until the loop is worth running again: a frame while a pop-out is opening, and
    /// the idle wait otherwise.
    fn next_wait(&self) -> Duration {
        if self.animating() {
            FRAME_WAIT
        } else {
            IDLE_WAIT
        }
    }

    /// Read output info: name, scale and logical size.
    fn refresh_output(&mut self, output: &wl_output::WlOutput) {
        let key = key_of(output);
        let info = self.output_state.info(output);
        let reported_name = self.name_of(output);
        if !self.outputs.contains_key(&key) {
            self.create_surface(output.clone());
        }
        // Some compositor updates briefly omit the name; do not regress a known bucket to the
        // temporary id-based fallback and repaint the wrong annotations.
        let name = if reported_name.is_empty() {
            self.outputs
                .get(&key)
                .map(|out| out.name.clone())
                .unwrap_or_default()
        } else {
            reported_name
        };
        let name_changed = self.outputs.get(&key).is_some_and(|out| out.name != name);
        if name_changed {
            // Finish under the old bucket before changing the name used by the document.
            self.end_text_edit();
            self.invalidate_text_caches();
        }
        self.adopt_scale(&key, output_scale(info.as_ref()));
        let Some(out) = self.outputs.get_mut(&key) else {
            return;
        };
        if out.name != name {
            out.name = name;
            out.dirty = true;
            self.dirty = true;
        }
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
        out.cache.invalidate();
        out.buffers.clear();
        out.last_buffer = None;
        out.last_transient = None;
        out.pending = None;
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
            out.pending = None;
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
        // previous buffer is in flight we fell back to the other one, so fill everything.
        // Every box below is a **screen** box: the overlay's geometry is canvas, so it is mapped
        // through this output's view once, here, and everything downstream — the cached boxes, the
        // damage region, the union — is screen-space like the raster it damages.
        let view = out.view;
        // The handles are interface lengths: on screen they are the constants themselves.
        let handle_hit = crate::canvas::SELECT_HANDLE_HIT;
        let transient = transient_bounds(&out.overlay).map(|b| view.screen_rect(b));
        let bucket = out.bucket(id);
        let empty = OutputAnnotations::default();
        let ann = self.doc.outputs.get(bucket.as_ref()).unwrap_or(&empty);
        // The frame and its handles are not document elements, so they are only ever drawn live; the
        // box they occupy is what the frame that repaints them has to damage. The handles are
        // interface-sized, so the frame is mapped first and grown on screen.
        let selected = out
            .overlay
            .selection
            .as_ref()
            .and_then(|s| selection_frame(ann, &s.items, s.transform));
        let transient = match (transient, selected) {
            (Some(a), Some(b)) => Some(a.union(selection_bounds(view.screen_rect(b), handle_hit))),
            (a, b) => a.or(b.map(|b| selection_bounds(view.screen_rect(b), handle_hit))),
        };
        let current_text_bounds = out.overlay.text.as_ref().and_then(|edit| {
            (edit.preedit.is_empty()).then(|| {
                view.screen_rect(self.renderer.text_edit_bounds(
                    &edit.buffer.text,
                    &edit.item,
                    Screen { scale, view },
                ))
            })
        });
        let edit_index_valid = out
            .overlay
            .text
            .as_ref()
            .and_then(|edit| edit.index)
            .is_none_or(|index| index < ann.texts.len());
        // A text box is a fixed box whose glyphs are all the frame has to repaint, so a keystroke in it
        // damages the box. A note is not: its square is document geometry that the text wraps into and
        // grows with, and it is left out of the raster, so a keystroke there has to repaint the square.
        // A box damage would leave the grown part of the square, and every line the text wrapped onto
        // below it, unpainted until something else caused a whole-surface frame — which is what typing
        // English, with no preedit to force one, looked like: no wrapping and no growth until the edit
        // ended.
        let local_text = out.text_dirty
            && out
                .overlay
                .text
                .as_ref()
                .is_none_or(|edit| edit.note.is_none())
            && !out.dirty
            && out.last_buffer == Some(idx)
            && out.last_text_bounds.is_some()
            && current_text_bounds.is_some()
            && edit_index_valid;
        let text_damage = local_text.then(|| {
            // The overlay paints the whole box of the item, which can be a little wider than the
            // region a click is tested against, so the box has to hold both.
            out.last_text_bounds
                .unwrap()
                .union(current_text_bounds.unwrap())
                .union(view.screen_rect(out.overlay.text.as_ref().unwrap().item.paint_bounds()))
        });
        let text_needs_full = out.overlay.text.is_some() && out.text_dirty && !local_text;
        // The raster never holds the box under edit, and it never holds the elements a live
        // gesture is dragging: the overlay draws each of them, in its live position, so a text
        // that has become shorter or an element that has moved cannot leave the old pixels behind.
        let mut skip: Vec<ItemId> = Vec::new();
        if let Some(edit) = out.overlay.text.as_ref() {
            // The item under edit is left out of the raster whichever list it belongs to: a note that
            // has grown, or a text box that has become shorter, would otherwise leave its committed
            // pixels behind, painted from the raster under the live one.
            match edit.note {
                Some(index) => skip.push(ItemId::Note(index)),
                None if edit_index_valid => {
                    if let Some(index) = edit.index {
                        skip.push(ItemId::Text(index));
                    }
                }
                None => {}
            }
        }
        if let Some(selection) = &out.overlay.selection {
            skip.extend_from_slice(selection.live());
        }
        let damage = if out.dirty || out.last_buffer != Some(idx) || text_needs_full {
            None
        } else if let Some(pending) = out.pending {
            // A document change with a known box: the changed element is in the rebuilt raster,
            // and outside the box the previous frame is still right, so the box is enough. The box the
            // document reported is canvas; this region is screen.
            let mut box_ = view.screen_rect(pending);
            for extra in [out.last_transient, transient].into_iter().flatten() {
                box_ = box_.union(extra);
            }
            if out.toolbar_dirty {
                box_ = box_.union(toolbar_bounds(w as f32, h as f32));
            }
            Some(box_)
        } else if let Some(text_damage) = text_damage {
            Some(text_damage)
        } else if out.transient_dirty || out.toolbar_dirty {
            // The rail's box is fixed while editing, so the previous and the current frame agree
            // on it and there is nothing to remember between frames.
            let rail = out
                .toolbar_dirty
                .then(|| toolbar_bounds(w as f32, h as f32));
            Some(transient_damage(out.last_transient, transient, rail))
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
        out.toolbar_dirty = false;
        out.text_dirty = false;
        out.pending = None;
        out.last_transient = transient;
        out.last_text_bounds = current_text_bounds;
        out.last_buffer = Some(idx);

        {
            let buffer = &mut out.buffers[idx];
            let Some(canvas) = buffer.canvas(&mut self.pool) else {
                return;
            };
            // Buffers are reused. The renderer fills the region this frame repaints from the
            // output's document raster, which is what makes the buffer's other bytes irrelevant.
            self.renderer.render_with_cache(
                canvas,
                px.0,
                px.1,
                scale,
                out.view,
                ann,
                &out.overlay,
                damage,
                &mut out.cache,
                &skip,
            );
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

/// How far the pop-out of the block under the pointer has opened, 0 to 1. A block with no
/// pointer on it is never drawn stretched, so its progress is simply complete: the collapse is
/// immediate on purpose, and the pop-out is the one direction this transition animates.
pub fn hover_grow(started: Option<Instant>, now: Instant) -> f32 {
    let Some(started) = started else {
        return 1.0;
    };
    let elapsed = now.saturating_duration_since(started);
    ease_out((elapsed.as_secs_f32() / TOOLBAR_ANIM.as_secs_f32()).clamp(0.0, 1.0))
}

/// The curve of the pop-out: fast at first and settling, so the stretch reads as a response to
/// the pointer rather than as a queue of frames. Apple's default easing decelerates.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// Longest selection the daemon reads. A clipboard belongs to another client, so a paste may
/// not be able to grow this one without bound.
pub const MAX_PASTE_BYTES: u64 = 256 * 1024;

/// The plain-text type to ask a selection for, best first: what GTK and Qt offer first, then the
/// `UTF8_STRING` older toolkits also publish, then plain `text/plain`.
pub fn text_mime(mime_types: &[String]) -> Option<String> {
    const WANTED: [&str; 3] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"];
    WANTED
        .iter()
        .find(|want| {
            mime_types
                .iter()
                .any(|offered| offered.eq_ignore_ascii_case(want))
        })
        .map(|want| (*want).to_string())
}

/// Read a selection as text: capped, and without CR, so pasted CRLF text leaves no invisible
/// characters in the box. Invalid UTF-8 is replaced rather than dropped.
pub fn read_selection(reader: impl Read) -> String {
    let mut bytes = Vec::new();
    let _ = reader.take(MAX_PASTE_BYTES).read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).replace('\r', "")
}

/// The pointer shape of a tool, as `wp_cursor_shape_v1` names it. The eraser takes the closest
/// named shape to a region eraser; there are no custom cursor images.
pub fn tool_cursor(tool: Tool) -> CursorShape {
    match tool {
        // Every tool that draws where the pointer is gets the crosshair, the shape tools included.
        Tool::Pen | Tool::Circle | Tool::Rect | Tool::Diamond | Tool::Line | Tool::Arrow => {
            CursorShape::Crosshair
        }
        Tool::Eraser => CursorShape::Cell,
        Tool::Text => CursorShape::Text,
        // The hand tool's gesture is a pan, and the grab shapes say exactly that.
        Tool::Hand => CursorShape::Grab,
        // The note tool reads as text, because the next thing that happens is typing.
        Tool::Note => CursorShape::Text,
        // The selection tool creates nothing of its own, so the pointer stays the plain arrow
        // until it is over something it would act on.
        Tool::Selection => CursorShape::Default,
    }
}

/// The pointer shape a resize handle takes: the protocol's own diagonal or edge shape for the
/// direction it drags in.
pub fn handle_cursor(handle: Handle) -> CursorShape {
    match handle {
        Handle::Nw | Handle::Se => CursorShape::NwseResize,
        Handle::Ne | Handle::Sw => CursorShape::NeswResize,
        Handle::N | Handle::S => CursorShape::NsResize,
        Handle::E | Handle::W => CursorShape::EwResize,
    }
}

fn key_of(output: &wl_output::WlOutput) -> u32 {
    output.id().protocol_id()
}

/// The output's scale factor, never below 1.
fn output_scale(info: Option<&OutputInfo>) -> f32 {
    info.map(|i| i.scale_factor.max(1) as f32).unwrap_or(1.0)
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
        let key = key_of(&output);
        if self.outputs.contains_key(&key) {
            self.end_text_edit();
            self.cancel_transient(key);
        }
        if self.keyboard_focus == Some(key) {
            self.keyboard_focus = None;
            self.sync_text_input();
        }
        if let Some(out) = self.outputs.remove(&key) {
            log(&format!("output {} removed, annotations kept", out.name));
        }
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, layer: &LayerSurface) {
        if let Some(key) = self.key_of_layer(layer) {
            self.end_text_edit();
            self.cancel_transient(key);
            if self.keyboard_focus == Some(key) {
                self.keyboard_focus = None;
                self.sync_text_input();
            }
            if let Some(old) = self.outputs.remove(&key) {
                log(&format!(
                    "compositor closed the layer surface of {}, recreating",
                    old.name
                ));
                self.create_surface(old.output.clone());
            }
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

    fn new_seat(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        // One data device per seat: it is what the compositor tells about the clipboard, and
        // what a paste reads the selection from.
        self.data_device = Some(self.data_device_manager.get_data_device(qh, &seat));
    }

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            // The shape device is bound to this pointer and is what makes the pointer follow
            // the active tool; without the protocol the compositor keeps its own cursor.
            if let Ok(pointer) = self.seat_state.get_pointer(qh, &seat) {
                self.cursor_device = self
                    .cursor_shape_manager
                    .as_ref()
                    .map(|manager| manager.get_pointer(&pointer, qh, GlobalData));
                // The pinch belongs to this pointer, and sctk does not wrap the protocol, so it is
                // fetched by hand the way the text input object is.
                self.pinch = self
                    .gestures
                    .as_ref()
                    .map(|gestures| gestures.get_pinch_gesture(&pointer, qh, PinchData));
            }
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

impl DataDeviceHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataDevice,
        _: f64,
        _: f64,
        _: &wl_surface::WlSurface,
    ) {
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}

    /// The compositor advertises the current selection; keep it for the next paste.
    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        self.selection = self
            .data_device
            .as_ref()
            .and_then(|device| device.data().selection_offer());
    }

    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
}

impl DataOfferHandler for App {
    /// Drag and drop is not supported: the rail has no drag sources and takes no drops.
    fn source_actions(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }

    fn selected_action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
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
