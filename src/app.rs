//! Wayland glue: layer surface lifecycle, mode switching, frame submission.

use std::collections::{HashMap, VecDeque};
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
    Attachment, Backdrop, Board, BoardEdit, BoardRow, ClipEntry, ClipRow, Doc, ERASER_THRESHOLD,
    Gesture, Grab, Handle, ItemId, LiveConnector, Motion, Note, Overlay, PEN_WIDTH_MAX,
    PEN_WIDTH_MIN, PEN_WIDTH_STEP, Press, Rect, SNAP_RADIUS, Selection, Shape, ShapeKind,
    TEXT_SIZE_MAX, TEXT_SIZE_MIN, TEXT_SIZE_STEP, TEXT_SNAP_RADIUS, Target, TextAttachment,
    TextBuffer, TextItem, TextOverlay, TextTarget, Toolbar, Transform, View, ZOOM_STEP, Zoom,
    date_stamp, link_source, marquee_select, note_drag, nudge_delta, record_clip, selection_bounds,
    selection_frame, selection_frame_with, selection_press_with, snap_square, stepped_size,
    toggle_selection,
};

pub use crate::canvas::{Tool, ToolbarAction};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape as CursorShape, WpCursorShapeDeviceV1,
};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1;
use wayland_protocols::wp::pointer_gestures::zv1::client::zwp_pointer_gesture_pinch_v1::ZwpPointerGesturePinchV1;
use wayland_protocols::wp::pointer_gestures::zv1::client::zwp_pointer_gestures_v1::ZwpPointerGesturesV1;
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::ZwpTextInputManagerV3;
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::ZwpTextInputV3;

use crate::input::{Nav, PinchData};
use crate::render::{
    DamageRegion, FrameCache, Renderer, Screen, buffer_format, move_button_bounds,
    snap_hint_bounds, toolbar_bounds, transient_bounds, transient_damage,
};
use crate::store::{self, Store};

/// Initial stroke width, in logical pixels.
pub const PEN_WIDTH: f32 = 3.0;
/// How far a paste lands from the copy it came from, in canvas pixels: enough to be seen on top.
pub const COPY_OFFSET: f32 = 12.0;

/// The distance the view moves by on an arrow press, in logical pixels: larger than a nudge, because a pan
/// of one pixel is not a pan.
pub const PAN_STEP: f32 = 40.0;
pub const PAN_STEP_FAR: f32 = 400.0;
/// Initial text size, in logical pixels.
pub const TEXT_SIZE: f32 = 18.0;

pub(crate) const BTN_LEFT: u32 = 0x110;
/// The wheel, pressed: a pan anchored at the point the press started on.
pub(crate) const BTN_MIDDLE: u32 = 0x112;
/// Max shm buffers kept per output: the compositor may still hold the previous one.
const MAX_BUFFERS: usize = 2;
/// How long the pop-out of a hovered block takes, one frame of it, and the relaxed poll the
/// event loop uses when nothing is moving.
const TOOLBAR_ANIM: Duration = Duration::from_millis(140);
/// How long the caret stays on, and then off. Slow enough not to pull the eye off the name.
const CARET_BLINK: Duration = Duration::from_millis(530);
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Toggle,
    Edit,
    Lock,
    Clear,
    /// Switch every output to the named board.
    Board(String),
    /// Create a board — one `Board N` when no name is given — and switch every output to it.
    NewBoard(Option<String>),
    /// List every board, with the one picked last marked.
    Boards,
    /// Whether every screen shows the board picked on any of them.
    Share(bool),
}

impl Command {
    pub fn parse(line: &str) -> Result<Self, String> {
        let line = line.trim();
        match line {
            "toggle" => Ok(Command::Toggle),
            "edit" => Ok(Command::Edit),
            "lock" => Ok(Command::Lock),
            "clear" => Ok(Command::Clear),
            "boards" => Ok(Command::Boards),
            "" => Err("empty command".into()),
            other => {
                if let Some(rest) = other.strip_prefix("board ") {
                    let rest = rest.trim();
                    if rest.is_empty() {
                        return Err("board needs a name, or 'board new'".into());
                    }
                    // `new` is a word of its own: a board called `newton` is a board.
                    let created = rest.strip_prefix("new").and_then(|after| match after {
                        "" => Some(""),
                        _ => after.strip_prefix(' '),
                    });
                    return Ok(match created {
                        Some("") => Command::NewBoard(None),
                        Some(name) => Command::NewBoard(Some(name.trim().to_string())),
                        None => Command::Board(rest.to_string()),
                    });
                }
                if let Some(rest) = other.strip_prefix("share ") {
                    return match rest.trim() {
                        "on" => Ok(Command::Share(true)),
                        "off" => Ok(Command::Share(false)),
                        _ => Err("share takes 'on' or 'off'".into()),
                    };
                }
                if other == "share" {
                    return Err("share takes 'on' or 'off'".into());
                }
                Err(format!("unknown command '{other}'"))
            }
        }
    }
}

pub struct Output {
    output: wl_output::WlOutput,
    /// The `wl_output` name; it may only arrive after the surface was created.
    name: String,
    /// The board this output shows. It is the document's key for everything drawn here, which is what
    /// lets two outputs read one board and lets a board outlive the output that was showing it.
    board: String,
    /// Whether that board was chosen rather than adopted. A name that arrives after the surface was made
    /// may still put the output back on the board named after it — which is how a file written before
    /// boards existed gives each screen its own drawing back — but never moves a board somebody picked.
    board_picked: bool,
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
    /// How fast the pointer is moving over this output, which decides the rung its frames are drawn
    /// at. Smoothed, so one slow event between fast ones does not flip the resolution.
    motion: Motion,
    /// The last pointer position seen on this output, and when, which is what `motion` measures the
    /// speed from. Per output, so crossing between outputs is not read as one enormous jump.
    motion_from: Option<[f32; 2]>,
    motion_at: Option<Instant>,
    /// The zoom this output is transitioning to, while one is in flight. It is not part of the view
    /// because it is not part of where the view *is*: the view is what every frame and every press
    /// reads, and this is only what the loop owes the next frames of animation.
    zoom: Option<Zoom>,
    cache: FrameCache,
}

impl Output {
    /// The rung this output's document is drawn at: full resolution unless the pointer is moving fast
    /// over it, which is what keeps a dragged or zoomed canvas at a frame time the eye reads as
    /// motion. It is a property of drawing only — nothing the view resolves through changes.
    fn quality(&self) -> f32 {
        self.motion.quality()
    }

    /// The board this output shows, which is what the document is read through.
    pub(crate) fn board(&self) -> &str {
        &self.board
    }

    /// How this output draws: its scale and the view its document is read through. The interface
    /// measures text through it, so a frame and the handles on it agree about the glyphs.
    pub(crate) fn screen(&self) -> Screen {
        Screen {
            scale: self.scale,
            view: self.view,
        }
    }
}

/// Whether the caret is on at `elapsed` since the edit began: on for one blink, off for the next.
fn caret_visible(elapsed: Duration) -> bool {
    (elapsed.as_millis() / CARET_BLINK.as_millis()).is_multiple_of(2)
}

/// A board's name being edited in the rail's list.
pub struct Rename {
    /// The row's place in the rail's list, which is stable while the keyboard is on it.
    pub(crate) index: usize,
    /// When the edit began, which is what the caret blinks from.
    pub(crate) since: Instant,
    /// The name the row had: the key the rename moves.
    pub(crate) board: String,
    pub(crate) text: TextBuffer,
    /// Input method preedit string; never persisted.
    pub(crate) preedit: String,
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

    pub(crate) renderer: Renderer,
    pub(crate) doc: Doc,
    /// Document snapshots taken before each user-visible change, newest last. In memory only: a
    /// restart begins without a history, and there is no redo.
    pub(crate) undo: UndoHistory,
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
    /// A connector end that a shape has magnetically met during the drag, written when the gesture
    /// lands. Indices are valid until the gesture ends, like the selection's.
    pub(crate) pending_links: Vec<PendingLink>,
    /// The drawing a dragged text selection has snapped to, written as a persistent attachment when
    /// the gesture lands. `None` while the text is not attached to anything.
    pub(crate) pending_text_snap: Option<TextTarget>,
    /// Whether the drawer of rarely used controls is open. Transient, like the selection.
    pub(crate) more: bool,
    /// What edit mode paints behind the document. A preference about reading, so it is not persisted.
    pub(crate) backdrop: Backdrop,
    /// Whether every screen shows the board picked on any of them, or each picks its own. Session state
    /// for the same reason: it is a choice about how this sitting reads. Per screen is the default, so
    /// an upgrade moves nobody's drawing.
    pub(crate) shared: bool,
    /// Whether the rail's boards panel is open. One panel at a time: opening it closes the drawer.
    pub(crate) boards_open: bool,
    /// Whether the rail's width list is open. The readout toggles it; one at a time like the others.
    pub(crate) size_open: bool,
    /// Whether the clipboard history panel is open.
    pub(crate) clipboard_open: bool,
    /// What a pick in the board list applies instead of switching the screen.
    pub(crate) target: Option<Target>,
    /// The in-app clipboard history, newest first. Session state: it is never written to the file.
    pub(crate) clips: Vec<ClipEntry>,
    /// The board's name being edited in the list, if any. A name is typed where it is read, so this is
    /// app-level state and not an output's.
    pub(crate) rename: Option<Rename>,
    /// The board whose row is asking whether to delete it. The name survives a list re-sort; it is
    /// cleared when the panel closes, a pick is made, or `no` is chosen.
    pub(crate) delete_confirm: Option<String>,
    /// When the open panel opened, for its own transition: `None` when no panel is open. Panels close at
    /// once, so only the opening has a clock.
    panel_open_since: Option<Instant>,
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
    /// When the previous tick ran, which is the `dt` a zoom in flight is stepped by.
    last_tick: Instant,
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

/// A connector end that a moved shape has come within the magnetic radius of. Applied to the
/// document only when the drag is released, so an abandoned drag changes nothing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PendingLink {
    pub connector: ItemId,
    pub end: usize,
    pub attachment: Attachment,
}

/// The copy an `Alt` drag made: what was added to the document and what it was copied from, so `Esc` can
/// put the document and the selection back where the press found them.
struct Duplicate {
    key: u32,
    copies: Vec<ItemId>,
    originals: Vec<ItemId>,
}

/// Most document snapshots the in-memory undo history keeps. An older one is dropped when a new
/// change arrives, so a long session costs a bounded amount of memory.
pub const UNDO_LIMIT: usize = 64;

/// The document's undo history: snapshots taken before user-visible changes to [`Doc`].
///
/// The snapshot is always taken *before* the mutation. A gesture and a text edit keep their
/// snapshot aside until they land, so one drag or one typing session is one undo step. Undo only
/// takes a snapshot out; it never puts one in, which is what keeps undo itself out of the history.
#[derive(Debug, Default)]
pub struct UndoHistory {
    snapshots: VecDeque<Doc>,
    /// Taken when a pointer gesture began: recorded once, when the gesture lands.
    gesture: Option<Doc>,
    /// Taken when a text edit began: recorded once, when the edit ends.
    edit: Option<Doc>,
}

impl UndoHistory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of changes that can still be undone.
    pub fn len(&self) -> usize {
        self.snapshots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }

    fn push(&mut self, snapshot: Doc) {
        self.snapshots.push_back(snapshot);
        while self.snapshots.len() > UNDO_LIMIT {
            self.snapshots.pop_front();
        }
    }

    /// Keep the document as it is before a change that is known to happen.
    pub fn checkpoint(&mut self, doc: &Doc) {
        self.push(doc.clone());
    }

    /// Keep `before` only if the document actually changed: a shift-click or a recolour of
    /// nothing must not leave an undo step behind.
    pub fn record(&mut self, before: Doc, after: &Doc) {
        if before != *after {
            self.push(before);
        }
    }

    /// Remember the document as a pointer gesture starts. The first snapshot wins, because one
    /// press is one gesture even when it makes several document changes (an `Alt`-drag copy and the
    /// move that follows it).
    pub fn begin_gesture(&mut self, doc: &Doc) {
        self.gesture.get_or_insert_with(|| doc.clone());
    }

    /// Record the gesture's starting document if it changed anything. A gesture that was abandoned
    /// with `Esc` never lands, so its snapshot is discarded instead.
    pub fn commit_gesture(&mut self, doc: &Doc) {
        if let Some(before) = self.gesture.take() {
            self.record(before, doc);
        }
    }

    /// Forget a gesture's snapshot without recording it: the gesture was taken back.
    pub fn abandon_gesture(&mut self) {
        self.gesture = None;
    }

    /// Remember the document as a text box or note is opened for writing.
    pub fn begin_edit(&mut self, doc: &Doc) {
        self.edit.get_or_insert_with(|| doc.clone());
    }

    /// Record the state before typing if the edit changed the document. Ending an edit without
    /// typing is not a change and leaves no step.
    pub fn commit_edit(&mut self, doc: &Doc) {
        if let Some(before) = self.edit.take() {
            self.record(before, doc);
        }
    }

    /// Take out the newest snapshot, for restoring. This adds nothing: undo is not itself a change
    /// to the document. Any pending gesture or edit snapshot is dropped, because the document they
    /// described no longer exists.
    pub fn undo(&mut self) -> Option<Doc> {
        self.gesture = None;
        self.edit = None;
        self.snapshots.pop_back()
    }
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
        // Configuration is startup state: read before the palette or the note border is used.
        crate::config::init();
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
            undo: UndoHistory::new(),
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
            shared: false,
            boards_open: false,
            size_open: false,
            clipboard_open: false,
            target: None,
            clips: Vec::new(),
            rename: None,
            delete_confirm: None,
            panel_open_since: None,
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
            pending_links: Vec::new(),
            pending_text_snap: None,
            more: false,
            backdrop: Backdrop::Black,
            pan: None,
            gestures: None,
            pinch: None,
            pointer_at: None,
            pinch_state: None,
            last_tick: Instant::now(),
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

    /// Logical size of one output, for the interface geometry that lives on the canvas.
    pub(crate) fn output_size(&self, key: u32) -> Option<(f32, f32)> {
        self.outputs
            .get(&key)
            .map(|out| (out.width as f32, out.height as f32))
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
        // Which board this output shows, decided once: the board picked last, or the one named after
        // this output (which is what a file written before boards existed stored its bucket as), or a
        // fresh one. A name that arrives later does not move the output: a stroke drawn meanwhile
        // belongs to the board it was drawn on, and the old fallback bucket lost exactly that stroke.
        let board = self.doc.adopt(&name);
        self.outputs.insert(
            key_of(&output),
            Output {
                output,
                name,
                board,
                board_picked: false,
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
                overlay: Overlay::default(),
                view: View::default(),
                motion: Motion::default(),
                motion_from: None,
                motion_at: None,
                zoom: None,
                cache: FrameCache::default(),
            },
        );
        // The rail is built after the output is in the map, because the boards panel marks the board *this*
        // output shows: built before, every row came out unmarked and the panel named no current board
        // until the pointer crossed it.
        self.refresh_toolbar();
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
        self.commit_rename();
        self.commit_any_gesture();
        self.cancel_transients();
        // A menu belongs to the mode it was opened in: locking hides the rail, and returning to edit mode
        // must not put back a list nobody asked for.
        self.close_panels();
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
            let (was_configured, name, board, board_picked, view, output) = (
                old.configured,
                old.name.clone(),
                old.board.clone(),
                old.board_picked,
                old.view,
                old.output.clone(),
            );
            drop(old);
            self.create_surface(output);
            if let Some(out) = self.outputs.get_mut(&key) {
                out.configured = was_configured;
                out.name = name;
                // The board this output was showing is what it goes on showing: a new surface adopts a
                // board for a screen that has just appeared, and this is not that.
                out.board = board;
                out.board_picked = board_picked;
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

    /// The edit-mode toolbar of the current mode, tool, colour and size, for one output: the boards
    /// panel marks the board *that* output shows, and in per-screen mode two rails show two boards.
    fn current_toolbar(&self, key: u32) -> Option<Toolbar> {
        match self.mode {
            Mode::Locked => None,
            Mode::Edit => {
                let board = self.outputs.get(&key).map(|out| out.board.as_str());
                Some(Toolbar {
                    tool: self.tool,
                    color: crate::config::current().colors[self.color_idx].clone(),
                    size: self.active_size(),
                    hover: self.toolbar_hover,
                    grow: self.toolbar_grow(Instant::now()),
                    panel_grow: self.panel_grow(Instant::now()),
                    more: self.more,
                    boards_open: self.boards_open,
                    size_open: self.size_open,
                    clipboard_open: self.clipboard_open,
                    clips: self
                        .clips
                        .iter()
                        .map(|clip| ClipRow {
                            items: clip.items,
                            preview: clip.preview(),
                        })
                        .collect(),
                    target: self.target,
                    rename: self.rename.as_ref().map(|rename| BoardEdit {
                        index: rename.index,
                        text: rename.text.text.clone(),
                        cursor: rename.text.cursor,
                        preedit: rename.preedit.clone(),
                        caret: caret_visible(rename.since.elapsed()),
                    }),
                    delete_confirm: self.delete_confirm.clone(),
                    boards: self
                        .doc
                        .names()
                        .map(|name| BoardRow {
                            name: name.clone(),
                            current: Some(name.as_str()) == board,
                        })
                        .collect(),
                    shared: self.shared,
                    backdrop: self.backdrop,
                })
            }
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

    /// `[` and `]` step the active tool's size, clamped to its range, and — with a selection held — the
    /// size of what it holds. The toolbar shows the value, so it is repainted (a whole-surface frame, like
    /// a tool or colour change).
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
        self.step_selection_size(dir);
        self.refresh_toolbar();
    }

    /// A keyboard step on the canvas, from [`crate::input::nav_action`]. It acts on the output the keyboard
    /// is focused on, which is the output whose rail the same keys already act on.
    pub(crate) fn nav_key(&mut self, nav: Nav) {
        let Some(key) = self
            .keyboard_focus
            .filter(|key| self.outputs.contains_key(key))
        else {
            return;
        };
        match nav {
            Nav::Cycle { forward } => self.cycle_selection(key, forward),
            Nav::Nudge { dx, dy, far } => self.nudge(key, dx, dy, far),
            Nav::Zoom { dir } => self.zoom_key(key, dir),
            Nav::ResetView => self.view_changed(key, |view| *view = View::default()),
            Nav::Open => self.open_held(key),
        }
    }

    /// `Tab`/`Shift+Tab`: put the selection on the next or the previous element of the board_name. A gesture in
    /// flight is committed first, or its transform would be written onto a selection that no longer holds
    /// the elements it was dragging.
    fn cycle_selection(&mut self, key: u32, forward: bool) {
        self.commit_any_gesture();
        let Some(out) = self.outputs.get(&key) else {
            return;
        };
        let board_name = out.board().to_string();
        let held = out
            .overlay
            .selection
            .as_ref()
            .map(|s| s.items.clone())
            .unwrap_or_default();
        let empty = Board::default();
        let ann = self.doc.boards.get(&board_name).unwrap_or(&empty);
        let items = crate::canvas::cycle_selection(ann, &held, forward);
        self.set_selection(key, items, None);
    }

    /// An arrow key: with something held it moves every held element, and with nothing held it moves the
    /// view. A held element moves by a screen distance — the step divided by the zoom — because a keyboard
    /// step is the same gesture as a pointer's, so it has to be the same length to look at at every zoom.
    fn nudge(&mut self, key: u32, dx: i32, dy: i32, far: bool) {
        let Some((held, zoom)) = self.outputs.get(&key).map(|out| {
            (
                out.overlay
                    .selection
                    .as_ref()
                    .map(|s| s.items.clone())
                    .unwrap_or_default(),
                out.view.zoom,
            )
        }) else {
            return;
        };
        if held.is_empty() {
            // The arrow names the edge of the drawing the viewport moves towards, so the drawing itself
            // moves the other way: `Right` looks further right, as it does in `tmmpr`.
            let step = if far { PAN_STEP_FAR } else { PAN_STEP };
            let delta = [-dx as f32 * step, -dy as f32 * step];
            self.view_changed(key, |view| view.pan_by(delta));
            return;
        }
        let delta = nudge_delta(dx, dy, far, zoom);
        // A keyboard step is a gesture of its own: its snapshot is taken before the move, and the
        // write below records it once.
        self.undo.begin_gesture(&self.doc);
        if let Some(out) = self.outputs.get_mut(&key)
            && let Some(selection) = out.overlay.selection.as_mut()
        {
            selection.transform = Some(Transform::Translate {
                dx: delta[0],
                dy: delta[1],
            });
        }
        // The release path a drag takes: one document change, one damaged box and a note's text re-wrapped,
        // without a second way of moving an element to drift from it.
        self.write_gesture(key);
    }

    /// `+`/`-`: zoom about the centre of the output, which is the one anchor a keyboard has.
    fn zoom_key(&mut self, key: u32, dir: i32) {
        let Some(out) = self.outputs.get(&key) else {
            return;
        };
        let anchor = [out.width as f32 / 2.0, out.height as f32 / 2.0];
        self.zoom_about(key, anchor, ZOOM_STEP.powf(dir as f32));
    }

    /// `Enter`: open the held text box or note for writing, with the caret at its end. Only one element can
    /// be written in at a time, so anything else does nothing; and it creates nothing, because a box where
    /// there was none needs a point the keyboard has not got.
    fn open_held(&mut self, key: u32) {
        let Some(held) = self.outputs.get(&key).and_then(|out| {
            out.overlay
                .selection
                .as_ref()
                .map(|s| s.items.clone())
                .filter(|items| items.len() == 1)
        }) else {
            return;
        };
        match held[0] {
            ItemId::Note(index) => self.enter_note(&key, index),
            ItemId::Text(index) => {
                let Some(out) = self.outputs.get(&key) else {
                    return;
                };
                let board_name = out.board().to_string();
                // A point inside the box the renderer measured, which is what a click on it would pass:
                // the origin of the estimate is not necessarily inside it for a wide script.
                let Some(box_) = self
                    .doc
                    .boards
                    .get(&board_name)
                    .and_then(|ann| ann.texts.get(index))
                    .map(TextItem::paint_bounds)
                else {
                    return;
                };
                self.begin_text_edit(&key, box_.x + box_.w / 2.0, box_.y + box_.h / 2.0);
            }
            _ => {}
        }
    }

    /// Open or close the drawer. It changes which controls exist, not the rail's height, so the rail's
    /// own box is what the next frame damages.
    pub(crate) fn toggle_more(&mut self) {
        self.set_more(!self.more);
    }

    /// Open or close the drawer, the one place either happens: the list is a menu, so picking from it and
    /// leaving it close it too, and all three paths have to leave the same state behind. A block that no
    /// longer exists cannot still own the pointer, so the hover goes with the layout.
    pub(crate) fn set_more(&mut self, open: bool) {
        if self.more == open {
            return;
        }
        // One panel at a time: the rail declares one panel box, and two plates on screen would be two
        // lists the pointer could not be said to be reading from.
        self.boards_open = false;
        self.size_open = false;
        self.clipboard_open = false;
        self.more = open;
        self.delete_confirm = None;
        self.panel_open_since = open.then(Instant::now);
        self.toolbar_hover = None;
        self.toolbar_hover_since = None;
        self.refresh_toolbar();
    }

    /// Open or close the boards panel, closing the drawer with it for the reason `set_more` closes this one.
    pub(crate) fn set_boards(&mut self, open: bool) {
        if self.boards_open == open {
            return;
        }
        self.more = false;
        self.size_open = false;
        self.clipboard_open = false;
        self.boards_open = open;
        if !open {
            self.target = None;
            self.delete_confirm = None;
        }
        self.panel_open_since = open.then(Instant::now);
        self.toolbar_hover = None;
        self.toolbar_hover_since = None;
        self.refresh_toolbar();
    }

    /// Open or close the clipboard history panel. A pick pastes and closes it, like every other panel.
    pub(crate) fn set_clipboard(&mut self, open: bool) {
        if self.clipboard_open == open {
            return;
        }
        self.more = false;
        self.boards_open = false;
        self.size_open = false;
        self.clipboard_open = open;
        self.target = None;
        self.delete_confirm = None;
        self.panel_open_since = open.then(Instant::now);
        self.toolbar_hover = None;
        self.toolbar_hover_since = None;
        self.refresh_toolbar();
    }

    /// Dismiss every panel: what a press outside one, a press on another rail block and a mode switch do.
    /// The setters are exclusive, so only the one that changes anything repaints the rail.
    pub(crate) fn close_panels(&mut self) {
        let confirming = self.delete_confirm.take().is_some();
        self.set_more(false);
        self.set_boards(false);
        self.set_size(false);
        self.set_clipboard(false);
        self.target = None;
        if confirming {
            self.refresh_toolbar();
        }
    }

    pub(crate) fn toggle_boards(&mut self) {
        self.set_boards(!self.boards_open);
    }

    /// Open or close the width list. Unlike `set_boards` it keeps the hover: clearing it would make the
    /// frame that changed the list forget the block the pointer is on.
    pub(crate) fn set_size(&mut self, open: bool) {
        if self.size_open == open {
            return;
        }
        self.boards_open = false;
        self.clipboard_open = false;
        self.more = false;
        self.size_open = open;
        self.delete_confirm = None;
        self.panel_open_since = open.then(Instant::now);
        self.refresh_toolbar();
    }

    /// The block that owns the open panel, if any: its hit box is the corridor the pointer travels
    /// through to reach the panel's rows, and `None` means no panel is open.
    pub(crate) fn panel_owner(&self) -> Option<ToolbarAction> {
        if self.boards_open {
            Some(ToolbarAction::Boards)
        } else if self.size_open {
            Some(ToolbarAction::Size)
        } else if self.clipboard_open {
            Some(ToolbarAction::Clipboard)
        } else if self.more {
            Some(ToolbarAction::More)
        } else {
            None
        }
    }

    /// Step the edit-mode backdrop: transparent, grid, dots, dim, and round again. It is a preference
    /// about reading, not content: the document and the file are untouched, and every output's
    /// raster is dropped because the backdrop is what the raster is painted on.
    pub(crate) fn cycle_backdrop(&mut self) {
        self.set_backdrop(self.backdrop.next());
    }

    /// Pick one backdrop mode. The rail is refreshed because the active option and the drawer's
    /// label both read it, and every raster is dropped because the backdrop is what the raster is
    /// painted on.
    pub(crate) fn set_backdrop(&mut self, mode: Backdrop) {
        if self.backdrop == mode {
            return;
        }
        self.backdrop = mode;
        self.refresh_toolbar();
        self.invalidate_text_caches();
        // Every pixel of the surface is the backdrop or its absence, so the frame that follows is a
        // whole-surface one.
        self.mark_all_dirty();
    }

    /// Put one board on the screens the mode says: every one of them when the screens share a board,
    /// else only the output the pick came from. `acting` is `None` for the command line, which has no
    /// output to act on — and in locked mode no output has the keyboard — so it moves them all.
    ///
    /// A switch changes no annotation: it finishes the text edit and the drawing in hand (both belong to
    /// the board being left), abandons a gesture, which has not reached the document yet, and drops every
    /// moved output's raster. It never schedules a write.
    pub(crate) fn switch_board(&mut self, acting: Option<u32>, name: &str) -> bool {
        if !self.doc.boards.contains_key(name) {
            return false;
        }
        self.end_text_edit();
        self.commit_transient_drawing();
        let keys: Vec<u32> = match (self.shared, acting) {
            (false, Some(key)) => vec![key],
            _ => self.outputs.keys().copied().collect(),
        };
        // A gesture in flight is not in the document, and the duplicate an `Alt` drag made names indices on
        // the board being left: kept, the next `Esc` would remove an element of the board switched to.
        self.gesture = None;
        self.pending_duplicate = None;
        self.undo.abandon_gesture();
        for key in keys {
            self.cancel_transient(key);
            let Some(out) = self.outputs.get_mut(&key) else {
                continue;
            };
            if out.board == name {
                continue;
            }
            out.board = name.to_string();
            out.board_picked = true;
            out.cache.invalidate();
            out.dirty = true;
            out.pending = None;
        }
        self.doc.active = name.to_string();
        self.dirty = true;
        log(&format!("board switched to {name}"));
        self.refresh_toolbar();
        true
    }

    /// Create a board and show it. This *is* a document change — the board is content, and one that is
    /// not written is gone tomorrow — so unlike a switch it schedules the write.
    pub(crate) fn new_board(&mut self, acting: Option<u32>) -> String {
        self.undo.checkpoint(&self.doc);
        let name = self.doc.new_board();
        self.store.mark_dirty(Instant::now());
        self.switch_board(acting, &name);
        // A brand-new board has nothing in it, and the raster of the board it replaced is already gone.
        name
    }

    /// The board and elements the selection holds on one output, if any.
    fn held(&self, key: u32) -> Option<(String, Vec<ItemId>)> {
        let out = self.outputs.get(&key)?;
        let selection = out.overlay.selection.as_ref()?;
        (!selection.items.is_empty()).then(|| (out.board.clone(), selection.items.clone()))
    }

    /// The closed shape a connector end at a canvas point should stick to, in canvas units: the
    /// magnetic radius is an interface length, so it is divided by the zoom.
    pub(crate) fn snap_connector(
        &self,
        key: &u32,
        point: [f32; 2],
    ) -> Option<(Attachment, [f32; 2])> {
        let out = self.outputs.get(key)?;
        let ann = self.doc.boards.get(out.board())?;
        ann.snap(point, SNAP_RADIUS / out.view.zoom)
    }

    /// The linked text under a screen point on this output: the board it links to and the canvas
    /// point its centre sits on, which is where the view goes after the switch.
    pub(crate) fn linked_text_at(
        &mut self,
        key: &u32,
        x: f32,
        y: f32,
    ) -> Option<(String, [f32; 2])> {
        let out = self.outputs.get(key)?;
        let board_name = out.board.clone();
        let (scale, view) = (out.scale, out.view);
        let point = view.to_canvas([x, y]);
        let ann = self.doc.boards.get(&board_name)?;
        let renderer = &mut self.renderer;
        let text = ann.linked_at(point, |text| {
            renderer.text_edit_bounds(&text.text, text, Screen { scale, view })
        })?;
        let board = text.link.clone()?;
        let bounds = text.paint_bounds();
        let centre = [bounds.x + bounds.w / 2.0, bounds.y + bounds.h / 2.0];
        Some((board, centre))
    }

    /// Go to a linked text: switch this output to its board and put the text's position in the
    /// middle. The centre is measured before the switch, on the board the link was read from.
    pub(crate) fn follow_link(&mut self, key: &u32, board: &str, centre: [f32; 2]) {
        if !self.switch_board(Some(*key), board) {
            log(&format!("link names '{board}', which is not a board"));
            return;
        }
        let Some(out) = self.outputs.get(key) else {
            return;
        };
        let size = (out.width as f32, out.height as f32);
        self.view_changed(*key, |view| *view = view.centred_on(size, centre));
    }

    /// Open the board list as a target picker: the `link` row, the move button and the keyboard all
    /// come through here. The target is applied by the row press, not by opening the list.
    pub(crate) fn open_target(&mut self, acting: Option<u32>, target: Target) {
        let Some(key) = acting.or(self.keyboard_focus) else {
            log("a target needs a focused screen");
            return;
        };
        let Some((board_name, items)) = self.held(key) else {
            log("a target needs a selection");
            return;
        };
        if target == Target::Link {
            let Some(ann) = self.doc.boards.get(&board_name) else {
                return;
            };
            if link_source(ann, &items).is_none() {
                log("link needs exactly one text box selected");
                return;
            }
        }
        self.target = Some(target);
        self.delete_confirm = None;
        if self.boards_open {
            self.refresh_toolbar();
        } else {
            self.set_boards(true);
        }
    }

    /// Apply a board pick to the pending target instead of switching the screen.
    pub(crate) fn apply_target(&mut self, acting: Option<u32>, target: Target, board: &str) {
        match target {
            Target::Link => self.link_selected(acting, board),
            Target::Move => self.move_selected(acting, board),
        }
    }

    fn link_selected(&mut self, acting: Option<u32>, board: &str) {
        let Some(key) = acting.or(self.keyboard_focus) else {
            return;
        };
        let Some((source, items)) = self.held(key) else {
            return;
        };
        let before = self.doc.clone();
        let Some(ann) = self.doc.boards.get_mut(&source) else {
            return;
        };
        let Some(index) = link_source(ann, &items) else {
            return;
        };
        ann.texts[index].link = Some(board.to_string());
        self.undo.record(before, &self.doc);
        self.doc_changed();
        log(&format!("text linked to {board}"));
    }

    /// Move the held elements onto the board a target press picked. Two boards changed, so every
    /// raster goes and the selection, whose indices name the source board, is dropped.
    fn move_selected(&mut self, acting: Option<u32>, board: &str) {
        let Some(key) = acting.or(self.keyboard_focus) else {
            return;
        };
        let Some((source, items)) = self.held(key) else {
            return;
        };
        let before = self.doc.clone();
        if self.doc.move_items(&source, board, &items).is_none() {
            return;
        }
        self.undo.record(before, &self.doc);
        self.drop_selection();
        self.doc_changed();
        log(&format!("moved {} elements to {board}", items.len()));
    }

    /// Copy the held elements into the app's own clipboard history. The newest copy is the one
    /// `Ctrl+V` pastes; the panel lists the rest. An over-cap copy is refused whole. Returns
    /// whether an entry was recorded, which is what tells a cut it may remove the originals.
    pub(crate) fn copy_selection(&mut self) -> bool {
        let Some(key) = self.keyboard_focus else {
            return false;
        };
        let Some((board_name, items)) = self.held(key) else {
            log("copy needs a selection");
            return false;
        };
        let Some(ann) = self.doc.boards.get(&board_name) else {
            return false;
        };
        let Some(count) = record_clip(&mut self.clips, ann, &items) else {
            log("copy refused: too large for the clipboard");
            return false;
        };
        self.refresh_toolbar();
        log(&format!("copied {count} elements"));
        true
    }

    /// Cut the held elements: exactly what `Ctrl+C` copies enters the clipboard history, and the
    /// same selection is then removed as one document change. With nothing held this does nothing,
    /// and a copy the clipboard refuses removes nothing either, so a cut never loses content the
    /// clipboard did not take.
    pub(crate) fn cut_selection(&mut self) {
        let Some(key) = self.keyboard_focus else {
            return;
        };
        if self.held(key).is_none() {
            return;
        }
        if self.copy_selection() {
            self.delete_selection();
        }
    }

    /// Paste clipboard entry `index`, newest first, onto the board this screen shows, a little offset.
    pub(crate) fn paste_clip(&mut self, index: usize) {
        let Some(clip) = self.clips.get(index).map(|clip| clip.board.clone()) else {
            return;
        };
        let Some(key) = self.keyboard_focus else {
            log("paste needs a focused screen");
            return;
        };
        let Some(out) = self.outputs.get(&key) else {
            return;
        };
        let board_name = out.board.clone();
        let copy = clip.shifted(COPY_OFFSET, COPY_OFFSET);
        let count = copy.strokes.len() + copy.texts.len() + copy.shapes.len() + copy.notes.len();
        let before = self.doc.clone();
        let Some(ann) = self.doc.boards.get_mut(&board_name) else {
            return;
        };
        let (ids, _) = ann.append(&copy);
        if ids.is_empty() {
            return;
        }
        self.undo.record(before, &self.doc);
        self.set_selection(key, ids, None);
        self.doc_changed();
        log(&format!("pasted {count} elements"));
    }

    /// Paste the newest clipboard entry: what `Ctrl+V` does outside a text box.
    pub(crate) fn paste_newest(&mut self) {
        if self.clips.is_empty() {
            log("paste needs a clipboard entry");
            return;
        }
        self.paste_clip(0);
    }

    /// Open the clipboard history panel, if there is a history to choose from.
    pub(crate) fn open_clipboard(&mut self) {
        if self.clips.is_empty() {
            log("the clipboard is empty");
            return;
        }
        self.set_clipboard(true);
    }

    /// Delete a board by name, from the panel's row that owns it. Everything the board holds goes with
    /// it, so the deletion is a document change and schedules the write.
    ///
    /// The edit and the drawing in hand belong to the board being left, and both commit through
    /// `entry(...).or_default()`: run them before the removal, or the board comes back as a fresh empty
    /// one. Every screen that was showing it then moves to the board that takes its place, through
    /// `switch_board`, which is what drops each moved output's selection and raster.
    pub(crate) fn delete_board_named(&mut self, name: &str) {
        self.end_text_edit();
        self.commit_transient_drawing();
        // A delete that the document refuses — the last board, or a name that is gone — must not
        // leave an undo step behind, so it is checked before the snapshot is taken.
        if self.doc.boards.len() <= 1 || !self.doc.boards.contains_key(name) {
            return;
        }
        self.undo.checkpoint(&self.doc);
        let Some(successor) = self.doc.delete_board(name) else {
            return;
        };
        // The keys are taken before anything moves: in shared mode the first switch moves every output,
        // and a loop that read `self.outputs` while switching would see boards change under it.
        let keys: Vec<u32> = self
            .outputs
            .iter()
            .filter(|(_, out)| out.board == name)
            .map(|(key, _)| *key)
            .collect();
        for key in keys {
            self.switch_board(Some(key), &successor);
        }
        // `switch_board` never schedules a write — a switch is a decision about this sitting — but a
        // deleted board has to leave the file or it is back the next time the daemon starts.
        self.store.mark_dirty(Instant::now());
        log(&format!("board deleted: {name}"));
    }

    /// Start editing the name of the board at `index` of the rail's list.
    pub(crate) fn begin_rename(&mut self, index: usize, name: &str) {
        self.end_text_edit();
        self.commit_transient_drawing();
        self.delete_confirm = None;
        self.rename = Some(Rename {
            index,
            since: Instant::now(),
            board: name.to_string(),
            text: TextBuffer::new(name),
            preedit: String::new(),
        });
        self.dirty = true;
        self.sync_text_input();
        self.refresh_toolbar();
    }

    /// Take the edited name if the document can hold it; anything else leaves the board as it was. A
    /// refused name is dropped rather than reported: the rail has nowhere to say why.
    pub(crate) fn commit_rename(&mut self) {
        let Some(rename) = self.rename.take() else {
            return;
        };
        let name = rename.text.text.trim().to_string();
        if name != rename.board {
            self.rename_board(&rename.board, &name);
        }
        self.dirty = true;
        self.sync_text_input();
        self.refresh_toolbar();
    }

    /// Leave the name as it was.
    pub(crate) fn cancel_rename(&mut self) {
        if self.rename.take().is_none() {
            return;
        }
        self.dirty = true;
        self.sync_text_input();
        self.refresh_toolbar();
    }

    /// Rename a board and keep every screen that shows it on it. Unlike a pick, a name is document
    /// content, so it goes to the file.
    pub(crate) fn rename_board(&mut self, old: &str, new: &str) -> bool {
        let before = self.doc.clone();
        if !self.doc.rename_board(old, new) {
            return false;
        }
        self.undo.record(before, &self.doc);
        for out in self.outputs.values_mut() {
            if out.board == old {
                out.board = new.to_string();
                out.dirty = true;
            }
        }
        self.dirty = true;
        self.store.mark_dirty(Instant::now());
        log(&format!("board renamed: {old} -> {new}"));
        true
    }

    /// Switch the shared mode on or off. On adopts the focused output's board on every output — that is
    /// the drawing in front of the person switching, and the keyboard is where the pointer's picks act
    /// too — falling back to the first output when the compositor has given the keyboard to nobody. Off
    /// changes no board: the screens are equal at that instant and may diverge afterwards.
    pub(crate) fn set_shared(&mut self, on: bool) {
        let focused = self
            .keyboard_focus
            .and_then(|key| self.outputs.get(&key))
            .map(|out| out.board.clone());
        let first = self.outputs.values().next().map(|out| out.board.clone());
        let board = self.doc.shared_board(focused.as_deref(), first.as_deref());
        self.shared = on;
        // `switch_board` reads the mode to decide who moves, so the mode is set first: switched on, the
        // pick then reaches every screen.
        if let (true, Some(board)) = (on, board) {
            self.switch_board(None, &board);
            return;
        }
        self.refresh_toolbar();
    }

    pub(crate) fn toggle_shared(&mut self) {
        self.set_shared(!self.shared);
    }

    /// Push the current toolbar into every output's transient overlay. The rail lives in the
    /// left strip and the document does not change with it, so its box is the only thing the
    /// next frame has to damage; the rest of the surface comes back from the document raster.
    pub(crate) fn refresh_toolbar(&mut self) {
        let keys: Vec<u32> = self.outputs.keys().copied().collect();
        for key in keys {
            let toolbar = self.current_toolbar(key);
            if let Some(out) = self.outputs.get_mut(&key) {
                out.overlay.toolbar = toolbar;
                out.toolbar_dirty = true;
            }
        }
        self.dirty = true;
    }

    /// The pointer crossed onto another block, or the pop-out moved on: the rail stretches a
    /// different block, or the same one a little further. Only the rail's own box changed, and it
    /// is fixed while editing, so the frame can damage just that box instead of the whole
    /// surface.
    pub(crate) fn refresh_toolbar_hover(&mut self) {
        let keys: Vec<u32> = self.outputs.keys().copied().collect();
        for key in keys {
            let toolbar = self.current_toolbar(key);
            if let Some(out) = self.outputs.get_mut(&key) {
                out.overlay.toolbar = toolbar;
                out.toolbar_dirty = true;
            }
        }
        self.dirty = true;
    }

    /// How far the pop-out of the block under the pointer has opened, 0 to 1.
    fn toolbar_grow(&self, now: Instant) -> f32 {
        hover_grow(self.toolbar_hover_since, now)
    }

    /// How far the open panel has opened, 0 to 1.
    fn panel_grow(&self, now: Instant) -> f32 {
        hover_grow(self.panel_open_since, now)
    }

    /// Whether a pop-out is still opening, and so whether the loop has to come back for the next
    /// frame of it.
    fn animating(&self) -> bool {
        self.toolbar_grow(Instant::now()) < 1.0
            || self.panel_grow(Instant::now()) < 1.0
            || zoom_in_flight(self.outputs.values().map(|out| out.zoom))
            || self.outputs.values().any(|out| out.motion.quality() < 1.0)
    }

    /// The toolbar control under a pointer position on an output, if any.
    pub(crate) fn toolbar_hit(&mut self, key: &u32, x: f32, y: f32) -> Option<ToolbarAction> {
        let out = self.outputs.get(key)?;
        let toolbar = out.overlay.toolbar.clone()?;
        let (width, height) = (out.width as f32, out.height as f32);
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

    /// Restore the newest undo snapshot. Returns `false` when there is nothing to undo, which
    /// changes nothing — not even a dirty flag or a repaint.
    ///
    /// Every transient that names document indices (a selection, a live gesture, a text box) is
    /// dropped before the restore, and every output whose board is gone is moved to a board the
    /// restored document holds. The raster is invalidated, and the write is scheduled, because the
    /// file and the screen have to agree with the document again. No snapshot is taken: undo is not
    /// itself a change in the history, and there is no redo.
    pub(crate) fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.undo() else {
            return false;
        };
        self.gesture = None;
        self.selection_grab = None;
        self.pending_duplicate = None;
        self.pending_links.clear();
        self.note_origin = None;
        self.note_sized = false;
        self.delete_confirm = None;
        self.rename = None;
        self.im_preedit = None;
        self.im_commit.clear();
        self.im_delete = None;
        self.doc = previous;
        // A document with no boards is nowhere to draw, and every output must name a board the
        // restored document holds: an undo of "create the only board" or "delete this board" would
        // otherwise leave the screen pointing at a name that is gone.
        if self.doc.boards.is_empty() {
            self.doc.new_board();
        }
        if !self.doc.boards.contains_key(&self.doc.active) {
            self.doc.active = self.doc.boards.keys().next().cloned().unwrap_or_default();
        }
        let fallback = self.doc.active.clone();
        let known: Vec<String> = self.doc.boards.keys().cloned().collect();
        for out in self.outputs.values_mut() {
            // The selection and the edit name indices into the document that was replaced, and the
            // toolbar is rebuilt below from the restored document.
            out.overlay = Overlay::default();
            out.text_dirty = false;
            out.last_text_bounds = None;
            out.last_transient = None;
            out.pending = None;
            out.cache.invalidate();
            out.dirty = true;
            if !known.contains(&out.board) {
                out.board = fallback.clone();
                // The fallback is where undo put this screen; a late output name must not move it.
                out.board_picked = true;
            }
        }
        self.doc_changed();
        self.refresh_toolbar();
        self.sync_text_input();
        log("undo");
        true
    }

    /// Commit any in-progress stroke or shape before a tool change or a new pointer gesture.
    pub(crate) fn commit_transient_drawing(&mut self) {
        let mut pending = Vec::new();
        let mut shapes = Vec::new();
        let mut overlay_changed = false;
        for (key, out) in self.outputs.iter_mut() {
            if let Some(stroke) = out.overlay.stroke.take() {
                if !stroke.points.is_empty() {
                    pending.push((*key, out.board().to_string(), stroke));
                }
                overlay_changed = true;
            }
            if let Some(shape) = out.overlay.shape.take() {
                if !shape.is_empty() {
                    shapes.push((*key, out.board().to_string(), shape));
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
        // The snapshot is taken before the strokes reach the document, and one commit is one undo
        // step however many outputs drew during the gesture.
        let before = (!pending.is_empty() || !shapes.is_empty()).then(|| self.doc.clone());
        let mut committed_box: Option<Rect> = None;
        let committed = !pending.is_empty() || !shapes.is_empty();
        for (_, board_name, stroke) in pending {
            if let Some(box_) = stroke.paint_bounds() {
                committed_box = Some(match committed_box {
                    Some(pending) => pending.union(box_),
                    None => box_,
                });
            }
            let board = self.doc.boards.entry(board_name).or_default();
            board.strokes.push(stroke);
            if let Some(settled) = board.sync_connectors() {
                committed_box = Some(committed_box.map_or(settled, |box_| box_.union(settled)));
            }
        }
        for (_, board_name, shape) in shapes {
            let box_ = shape.paint_bounds();
            committed_box = Some(match committed_box {
                Some(pending) => pending.union(box_),
                None => box_,
            });
            let board = self.doc.boards.entry(board_name).or_default();
            board.add_shape(shape);
            if let Some(settled) = board.sync_connectors() {
                committed_box = Some(committed_box.map_or(settled, |box_| box_.union(settled)));
            }
        }
        if committed {
            if let Some(before) = before {
                self.undo.record(before, &self.doc);
            }
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
        self.pending_links.clear();
        self.pending_text_snap = None;
        let Some(out) = self.outputs.get_mut(&key) else {
            return;
        };
        let changed = out.overlay.stroke.take().is_some()
            || out.overlay.shape.take().is_some()
            || out.overlay.eraser.take().is_some()
            || out.overlay.note.take().is_some()
            || out.overlay.selection.take().is_some()
            || out.overlay.marquee.take().is_some();
        out.overlay.connectors.clear();
        out.overlay.attached_texts.clear();
        if changed {
            out.transient_dirty = true;
            out.dirty = true;
            self.dirty = true;
        }
    }

    /// With the selection tool armed, a press grabs a handle of the frame, takes the element under
    /// the pointer, or starts a marquee. A handle wins over the element below it, so a resize that
    /// starts on a corner never moves the selection instead. With `Shift` held the element under the
    /// pointer is toggled instead of taken and no gesture starts, and a press on nothing does nothing.
    pub(crate) fn select_press(&mut self, key: u32, x: f32, y: f32) {
        // The decision is `canvas::selection_press`, where the two coordinate spaces — the screen the
        // handles live in and the canvas a gesture moves the document in — are one function a test can
        // reach. This only carries it out.
        let (press, frame, at) = {
            let Some(out) = self.outputs.get(&key) else {
                return;
            };
            let view = out.view;
            let board_name = out.board().to_string();
            let empty = Board::default();
            let ann = self.doc.boards.get(&board_name).unwrap_or(&empty);
            let held = out
                .overlay
                .selection
                .as_ref()
                .map(|s| s.items.clone())
                .unwrap_or_default();
            // The handles are drawn on the measured text, so a press has to grab them there: the
            // estimate would leave them off the words it is meant to frame.
            let boxes = self.renderer.text_boxes(
                ann,
                &held,
                Screen {
                    scale: out.scale,
                    view,
                },
            );
            let press = selection_press_with(
                ann,
                &held,
                view,
                [x, y],
                view.canvas_len(ERASER_THRESHOLD),
                self.shift,
                &boxes,
            );
            let frame = selection_frame_with(ann, &held, None, &boxes);
            (press, frame, view.to_canvas([x, y]))
        };
        let alt = self.alt;
        match press {
            // A gesture works in the document's own coordinates, because that is what the transform it
            // produces is applied to.
            Press::Handle(grab) => {
                self.undo.begin_gesture(&self.doc);
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
                self.undo.begin_gesture(&self.doc);
                self.gesture = Some(Gesture::start(
                    None,
                    frame.unwrap_or_default(),
                    at[0],
                    at[1],
                    alt,
                ));
            }
            Press::Element(id) => {
                self.undo.begin_gesture(&self.doc);
                self.set_selection(key, vec![id], None);
                self.gesture = Some(Gesture::start(None, Rect::default(), at[0], at[1], alt));
            }
            Press::Marquee { canvas } => {
                // The marquee is a canvas rectangle: the frame it is drawn in and the elements it
                // catches are both canvas, and only the pointer has to be converted once, here.
                self.gesture = Some(Gesture::Marquee { origin: canvas });
            }
            // A `Shift` press is a click, not a drag: the selection changes on the press and no gesture
            // is started, so a `Shift` drag cannot move what it just toggled.
            Press::Toggle(id) => {
                let held = self
                    .outputs
                    .get(&key)
                    .and_then(|out| out.overlay.selection.as_ref())
                    .map(|s| s.items.clone())
                    .unwrap_or_default();
                self.set_selection(key, toggle_selection(&held, id), None);
            }
            Press::Inert => {}
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
        let Some(mut transform) = gesture.transform(at, self.shift) else {
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
        // A moved closed shape can meet a free connector end from its own side too. The correction
        // is part of the transform the frame draws, so the shape visibly lands on the end. A
        // selection made only of text instead snaps to the drawing its ink came near.
        let items = self
            .outputs
            .get(&key)
            .and_then(|out| out.overlay.selection.as_ref())
            .map(|selection| selection.items.clone())
            .unwrap_or_default();
        let text_only = !items.is_empty() && items.iter().all(|id| matches!(id, ItemId::Text(_)));
        let old_links = std::mem::take(&mut self.pending_links);
        let old_text_snap = self.pending_text_snap;
        if matches!(gesture, Gesture::Move { .. }) && !transform.is_identity() {
            if text_only {
                let snap = self.outputs.get(&key).and_then(|out| {
                    self.doc.boards.get(out.board()).and_then(|board| {
                        board.snap_text(&items, transform, TEXT_SNAP_RADIUS / out.view.zoom)
                    })
                });
                match snap {
                    Some(snap) => {
                        transform = snap.transform;
                        self.pending_text_snap = Some(snap.target);
                    }
                    None => self.pending_text_snap = None,
                }
            } else {
                self.pending_text_snap = None;
                let (snapped, links) = self.snap_moved_shape(&key, &items, transform);
                transform = snapped;
                self.pending_links = links;
            }
        } else {
            self.pending_links.clear();
            self.pending_text_snap = None;
        }
        let live_changed =
            old_links != self.pending_links || old_text_snap != self.pending_text_snap;
        let next = (!transform.is_identity()
            || !self.pending_links.is_empty()
            || self.pending_text_snap.is_some())
        .then_some(transform);
        let changed = {
            let Some(out) = self.outputs.get_mut(&key) else {
                return;
            };
            let Some(selection) = out.overlay.selection.as_mut() else {
                return;
            };
            if selection.transform == next {
                false
            } else {
                selection.transform = next;
                out.transient_dirty = true;
                self.dirty = true;
                true
            }
        };
        if changed || live_changed {
            self.refresh_live(key);
        }
    }

    /// While a closed shape is moved, let it meet a free connector end halfway: the translation is
    /// corrected so the shape's outline lands exactly on the end, and the attachment is kept for the
    /// release. Only a move translates; a resize or turn is not a magnetic gesture.
    fn snap_moved_shape(
        &self,
        key: &u32,
        items: &[ItemId],
        transform: Transform,
    ) -> (Transform, Vec<PendingLink>) {
        let Some(out) = self.outputs.get(key) else {
            return (transform, Vec::new());
        };
        let Some(board) = self.doc.boards.get(out.board()) else {
            return (transform, Vec::new());
        };
        match board.reverse_snap(items, transform, SNAP_RADIUS / out.view.zoom) {
            Some(snap) => (
                snap.transform,
                vec![PendingLink {
                    connector: snap.connector,
                    end: snap.end,
                    attachment: snap.attachment,
                }],
            ),
            None => (transform, Vec::new()),
        }
    }

    /// Rebuild the attached-connector and attached-text copies the overlay paints during a selection
    /// drag, and skip their committed geometry in the raster.
    fn refresh_live(&mut self, key: u32) {
        let Some((board_name, items, transform)) = self.outputs.get(&key).and_then(|out| {
            let selection = out.overlay.selection.as_ref()?;
            let transform = selection.transform?;
            Some((out.board().to_string(), selection.items.clone(), transform))
        }) else {
            if let Some(out) = self.outputs.get_mut(&key) {
                out.overlay.connectors.clear();
                out.overlay.attached_texts.clear();
            }
            return;
        };
        let pending: Vec<_> = self
            .pending_links
            .iter()
            .map(|link| (link.connector, link.end, link.attachment))
            .collect();
        let (connectors, attached_texts) = self
            .doc
            .boards
            .get(&board_name)
            .map(|board| {
                (
                    board.connector_preview(&items, transform, &pending),
                    board.attached_text_preview(&items, transform),
                )
            })
            .unwrap_or_default();
        if let Some(out) = self.outputs.get_mut(&key) {
            out.overlay.connectors = connectors;
            out.overlay.attached_texts = attached_texts;
            out.transient_dirty = true;
            self.dirty = true;
        }
    }

    /// Copy what the selection holds and hand the selection to the copies, so the gesture that is running
    /// drags the copy while the original stays where it was. The copy is recorded until the gesture is
    /// over, because `Esc` has to be able to take it back.
    fn duplicate_selection(&mut self, key: u32) {
        // The copy is part of the move gesture whose press took the snapshot; this only covers a
        // path that reaches here without one.
        self.undo.begin_gesture(&self.doc);
        let Some(out) = self.outputs.get(&key) else {
            return;
        };
        let Some(selection) = out.overlay.selection.as_ref() else {
            return;
        };
        let originals = selection.items.clone();
        let transform = selection.transform;
        let board_name = out.board().to_string();
        let Some((copies, box_)) = self
            .doc
            .boards
            .get_mut(&board_name)
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
        let board_name = out.board().to_string();
        let removed = self
            .doc
            .boards
            .get_mut(&board_name)
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
        self.undo.abandon_gesture();
        self.selection_grab = None;
        self.sync_cursor();
    }

    /// Record how fast the pointer is moving over an output, which is what decides the resolution its
    /// frames are drawn at. It takes the movement and the time since the last one on *this* output, so
    /// crossing between outputs does not read as one enormous jump.
    pub(crate) fn note_motion(&mut self, key: &u32, x: f32, y: f32) {
        let now = Instant::now();
        let Some(out) = self.outputs.get_mut(key) else {
            return;
        };
        let dt = out
            .motion_at
            .map_or(FRAME_WAIT.as_secs_f32(), |at: Instant| {
                now.saturating_duration_since(at).as_secs_f32().max(0.001)
            });
        let distance = out
            .motion_from
            .map_or(0.0, |p| ((x - p[0]).powi(2) + (y - p[1]).powi(2)).sqrt());
        out.motion_from = Some([x, y]);
        out.motion_at = Some(now);
        out.motion.moved(distance, dt);
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

    /// Zoom about a screen point, which stays where it is for the whole transition.
    pub(crate) fn zoom_about(&mut self, key: u32, anchor: [f32; 2], factor: f32) {
        let Some(out) = self.outputs.get(&key) else {
            return;
        };
        let canvas = out.view.to_canvas(anchor);
        self.zoom_request(key, canvas, anchor, factor);
    }

    /// Ask for a zoom that holds a given canvas point under a screen point. A pinch needs this shape,
    /// because the fingers both move and change their distance; a wheel and a key go through
    /// `zoom_about` and land here. A second request while one is in flight retargets it and leaves
    /// the zoom where it visibly is, so a burst is one transition and its last target wins.
    fn zoom_request(&mut self, key: u32, canvas: [f32; 2], anchor: [f32; 2], factor: f32) {
        let Some(out) = self.outputs.get_mut(&key) else {
            return;
        };
        let base = out.zoom.map_or(out.view.zoom, |zoom| zoom.target);
        out.zoom = Some(Zoom::request(out.view, base, canvas, anchor, factor));
        self.view_moved(key);
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
            self.zoom_request(key, canvas, anchor, factor);
        } else {
            // The fingers only moved: the canvas point they started on follows them, and a zoom
            // already in flight is held under the fingers' new position rather than the old one.
            self.view_changed(key, |view| {
                let zoom = view.zoom;
                view.offset = [anchor[0] - canvas[0] * zoom, anchor[1] - canvas[1] * zoom];
            });
            if let Some(out) = self.outputs.get_mut(&key)
                && let Some(zoom) = out.zoom.as_mut()
            {
                zoom.anchor = anchor;
            }
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
        if let Some(out) = self.outputs.get_mut(&key) {
            f(&mut out.view);
        }
        self.view_moved(key);
    }

    /// Note that an output's view has already changed: the boxes the previous frame cached describe a
    /// view that no longer exists, so they go and the frame that follows repaints whole.
    fn view_moved(&mut self, key: u32) {
        let Some(out) = self.outputs.get_mut(&key) else {
            return;
        };
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
        self.pending_links.clear();
        self.undo.abandon_gesture();
        self.pending_text_snap = None;
        let mut dropped = false;
        for out in self.outputs.values_mut() {
            let had = out.overlay.selection.take().is_some() | out.overlay.marquee.take().is_some();
            out.overlay.connectors.clear();
            out.overlay.attached_texts.clear();
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
                    .map(|s| (*key, out.board().to_string(), s.items.clone()))
            })
            .collect();
        let mut removed = Vec::new();
        let before = self.doc.clone();
        for (key, board_name, items) in held {
            if let Some(box_) = self
                .doc
                .boards
                .get_mut(&board_name)
                .and_then(|ann| ann.remove(&items))
            {
                removed.push((key, box_));
            }
        }
        let deleted = !removed.is_empty();
        if deleted {
            self.undo.record(before, &self.doc);
        }
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
        self.undo.abandon_gesture();
    }

    /// Hold `items` on one output and let go everywhere else: at most one selection exists at a
    /// time, the way at most one text box is under edit.
    fn set_selection(&mut self, key: u32, items: Vec<ItemId>, transform: Option<Transform>) {
        self.pending_links.clear();
        self.pending_text_snap = None;
        let owned =
            (!items.is_empty() || transform.is_some()).then_some(Selection { items, transform });
        for (k, out) in self.outputs.iter_mut() {
            let next = if *k == key { owned.clone() } else { None };
            if out.overlay.selection != next {
                out.overlay.selection = next;
                out.overlay.connectors.clear();
                out.overlay.attached_texts.clear();
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
        let board_name = self.outputs.get(&key).map(|out| out.board().to_string());
        let items = match board_name {
            Some(board_name) => {
                let empty = Board::default();
                let ann = self.doc.boards.get(&board_name).unwrap_or(&empty);
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
        let pending_text_snap = self.pending_text_snap.take();
        // A text that was snapped while the gesture never left its own origin still has an
        // attachment to write: the identity transform is the committed position.
        let transform = match selection.transform {
            Some(transform) => transform,
            None if pending_text_snap.is_some() => Transform::Translate { dx: 0.0, dy: 0.0 },
            None => return,
        };
        let (items, board_name) = (selection.items.clone(), out.board().to_string());
        let links = std::mem::take(&mut self.pending_links);
        let mut box_: Option<Rect> = None;
        if let Some(ann) = self.doc.boards.get_mut(&board_name) {
            for id in &items {
                let changed = match *id {
                    ItemId::Stroke(i) if i < ann.strokes.len() => {
                        let turned = transform.stroke(&ann.strokes[i]);
                        let changed = ann.strokes[i]
                            .paint_bounds()
                            .into_iter()
                            .chain(turned.paint_bounds())
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
                        let changed = ann.shapes[i].paint_bounds().union(moved.paint_bounds());
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
            // A shape that met a free end during the drag takes that end now: the attachment is written
            // first, then the same sync pass seats both endpoints on their shapes.
            for link in &links {
                let before = match link.connector {
                    ItemId::Stroke(i) if i < ann.strokes.len() => ann.strokes[i].paint_bounds(),
                    ItemId::Shape(i) if i < ann.shapes.len() => Some(ann.shapes[i].paint_bounds()),
                    _ => None,
                };
                box_ = match (box_, before) {
                    (Some(a), Some(b)) => Some(a.union(b)),
                    (a, b) => a.or(b),
                };
                match link.connector {
                    ItemId::Stroke(i) if i < ann.strokes.len() => {
                        ann.strokes[i].attach[link.end] = Some(link.attachment);
                    }
                    ItemId::Shape(i) if i < ann.shapes.len() => {
                        ann.shapes[i].attach[link.end] = Some(link.attachment);
                    }
                    _ => {}
                }
            }
            // A text that met a drawing takes that attachment now. The offset is stored from the
            // target's anchor, so the text follows the drawing through any later translation.
            if let Some((target, anchor)) = pending_text_snap
                .and_then(|target| ann.text_anchor(target).map(|anchor| (target, anchor)))
            {
                for id in &items {
                    if let ItemId::Text(i) = *id
                        && let Some(text) = ann.texts.get_mut(i)
                    {
                        text.attach = Some(TextAttachment {
                            target,
                            offset: [text.x - anchor[0], text.y - anchor[1]],
                        });
                    }
                }
            }
            // A moved text that met nothing is dragged away from what it was attached to. A text
            // whose target is in the same selection keeps its link, because both moved together.
            let selected_targets: Vec<TextTarget> = items
                .iter()
                .filter_map(|id| match *id {
                    ItemId::Shape(i) => ann
                        .shapes
                        .get(i)
                        .map(|shape| TextTarget::Shape(shape.id))
                        .filter(|_| ann.shapes[i].id != 0),
                    ItemId::Stroke(i) => ann
                        .strokes
                        .get(i)
                        .map(|stroke| TextTarget::Stroke(stroke.id))
                        .filter(|_| ann.strokes[i].id != 0),
                    _ => None,
                })
                .collect();
            for id in &items {
                if pending_text_snap.is_some() {
                    break;
                }
                if let ItemId::Text(i) = *id
                    && let Some(text) = ann.texts.get_mut(i)
                    && text
                        .attach
                        .is_some_and(|attachment| !selected_targets.contains(&attachment.target))
                {
                    text.attach = None;
                }
            }
            // A moved shape drags every connector attached to it, whether or not the connector was
            // part of the selection. Both endpoints are re-seated on their shapes here, once, on the
            // same commit as the move.
            if let Some(connector_box) = ann.sync_connectors() {
                box_ = Some(box_.map_or(connector_box, |box_| box_.union(connector_box)));
            }
            // The same pass for texts: a moved shape or stroke drags the text attached to it.
            if let Some(text_box) = ann.sync_attached_texts() {
                box_ = Some(box_.map_or(text_box, |box_| box_.union(text_box)));
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
            out.overlay.connectors.clear();
            out.overlay.attached_texts.clear();
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
                .boards
                .get(&board_name)
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
                    .boards
                    .get_mut(&board_name)
                    .and_then(|ann| ann.notes.iter_mut().find(|n| n.bounds() == note.bounds()))
                {
                    slot.h = h;
                }
            }
        }
        // Everything this gesture did — including the Alt-drag copy and the re-wrap above — is one
        // change, so its starting snapshot is recorded once, here.
        self.undo.commit_gesture(&self.doc);
    }

    fn cancel_transients(&mut self) {
        let keys: Vec<u32> = self.outputs.keys().copied().collect();
        for key in keys {
            self.cancel_transient(key);
        }
    }

    pub(crate) fn set_color(&mut self, idx: usize) {
        if idx < crate::config::current().colors.len() {
            self.color_idx = idx;
            self.refresh_toolbar();
            self.recolour_selection(&crate::config::current().colors[idx]);
        }
    }

    /// The other half of a size step: what the selection holds takes the step too, as one change, the way
    /// `recolour_selection` applies a colour chip to it. The list is taken first, because the boards are
    /// borrowed mutably in the loop — and two outputs may each hold a selection, on different boards.
    fn step_selection_size(&mut self, dir: f32) {
        let held: Vec<(u32, String, Vec<ItemId>)> = self
            .outputs
            .iter()
            .filter_map(|(key, out)| {
                out.overlay
                    .selection
                    .as_ref()
                    .map(|s| (*key, out.board().to_string(), s.items.clone()))
            })
            .collect();
        let mut changed = Vec::new();
        let before = self.doc.clone();
        for (key, board_name, items) in held {
            if let Some(box_) = self
                .doc
                .boards
                .get_mut(&board_name)
                .and_then(|ann| ann.step_size(&items, dir))
            {
                changed.push((key, box_));
            }
        }
        if !changed.is_empty() {
            self.undo.record(before, &self.doc);
        }
        for (key, box_) in changed {
            self.doc_changed_at(key, box_);
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
                    .map(|s| (*key, out.board().to_string(), s.items.clone()))
            })
            .collect();
        let mut changed = Vec::new();
        let before = self.doc.clone();
        for (key, board_name, items) in held {
            if let Some(box_) = self
                .doc
                .boards
                .get_mut(&board_name)
                .and_then(|ann| ann.recolour(&items, color))
            {
                changed.push((key, box_));
            }
        }
        if !changed.is_empty() {
            self.undo.record(before, &self.doc);
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
            let (scale, board_name) = (out.scale, out.board().to_string());
            let mut box_ = previous.unwrap_or_default();
            // A note being edited is content: its text goes back into the note, with the height the
            // wrapped text needs, and a note left empty is dropped as an empty text box is.
            if let Some(index) = edit.note {
                let note = self
                    .doc
                    .boards
                    .get(&board_name)
                    .and_then(|ann| ann.notes.get(index))
                    .cloned();
                let Some(mut note) = note else { continue };
                box_ = box_.union(view.screen_rect(note.bounds()));
                let text = edit.buffer.text.clone();
                // An empty note stays: an empty sticky is a sticky, and it is what the user dragged
                // out. Dropping it made a note the user had just placed vanish the moment they
                // started the next one, which reads as the note having failed to be created.
                if let Some(ann) = self.doc.boards.get_mut(&board_name) {
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
            let ann = self.doc.boards.entry(board_name).or_default();
            match edit.index {
                Some(idx) if idx < ann.texts.len() => ann.texts[idx] = item,
                _ => ann.texts.push(item),
            }
            changed = true;
        }
        if changed {
            self.doc_changed();
        }
        // The edit's starting document is recorded here, and only if typing changed anything.
        self.undo.commit_edit(&self.doc);
        self.im_preedit = None;
        self.im_commit.clear();
        self.im_delete = None;
        self.dirty = true;
        self.sync_text_input();
    }

    pub(crate) fn begin_text_edit(&mut self, key: &u32, x: f32, y: f32) {
        let Some(out) = self.outputs.get(key) else {
            return;
        };
        let board_name = out.board();
        let scale = out.scale;
        // The click is tested against the text the renderer measures, not against the font-size
        // estimate the eraser uses: that estimate is 0.6 em per character, which is too narrow
        // for CJK, so clicking such a glyph used to start a new box instead of editing it.
        let existing = self.doc.boards.get(board_name).and_then(|ann| {
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
            .boards
            .get(board_name)
            .and_then(|ann| ann.notes.iter().position(|n| n.bounds().contains(x, y)));
        if let Some(index) = on_note {
            self.enter_note(key, index);
            return;
        }
        let (item, index) = match existing {
            Some(idx) => (self.doc.boards[board_name].texts[idx].clone(), Some(idx)),
            None => (
                TextItem {
                    x,
                    y,
                    color: crate::config::current().colors[self.color_idx].clone(),
                    size: self.text_size,
                    text: String::new(),
                    link: None,
                    attach: None,
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
        // Typed content is one undo step: the document as it stands before the edit is what undo
        // brings back, however many keystrokes land.
        self.undo.begin_edit(&self.doc);
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
        let color = crate::config::current().colors[self.color_idx].clone();
        let width = self.pen_width;
        let snap = if matches!(kind, ShapeKind::Line | ShapeKind::Arrow) {
            self.snap_connector(key, [x, y])
        } else {
            None
        };
        let (from, attach) = match snap {
            Some((attachment, point)) => (point, [Some(attachment), None]),
            None => ([x, y], [None, None]),
        };
        let Some(out) = self.outputs.get_mut(key) else {
            return;
        };
        out.overlay.shape = Some(Shape {
            kind,
            color,
            width,
            from,
            to: from,
            rotate: 0.0,
            id: 0,
            attach,
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
        let Some(board_name) = self.outputs.get(&key).map(|out| out.board().to_string()) else {
            return;
        };
        let Some(note) = self
            .doc
            .boards
            .get(&board_name)
            .and_then(|ann| ann.notes.get(index))
            .cloned()
        else {
            return;
        };
        let needed = self
            .renderer
            .wrapped_text_height(&text, &note, Screen { scale, view });
        let updated = note.grown_for(needed);
        if let Some(ann) = self.doc.boards.get_mut(&board_name)
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
        let Some(out) = self.outputs.get(key) else {
            return;
        };
        let board_name = out.board().to_string();
        let existing = self
            .doc
            .boards
            .get(&board_name)
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
                &crate::config::current().colors[self.color_idx],
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
        let board_name = out.board().to_string();
        let box_ = note.bounds();
        // The note becomes content here, and that is the change undo brings the board back from.
        let before = self.doc.clone();
        let index = {
            let ann = self.doc.boards.entry(board_name).or_default();
            ann.notes.push(note);
            ann.notes.len() - 1
        };
        self.undo.record(before, &self.doc);
        // A note's own creation is a document change like any other, and it is the change the debounced save
        // hangs off: without this the note was on the screen and in memory but not in the store's dirty flag,
        // so a session that ended before the edit did — a crash, a `SIGKILL` — lost a note that had just been
        // dragged out. Every other commit path reports its box here; this one did not.
        self.doc_changed_at(*key, box_);
        self.enter_note(key, index);
    }

    /// Edit a note's text through the same overlay a text box uses, with the overlay naming the note so
    /// that the caret, the preedit, the input method and the commit all know where the text belongs.
    fn enter_note(&mut self, key: &u32, index: usize) {
        let Some(out) = self.outputs.get(key) else {
            return;
        };
        let board_name = out.board().to_string();
        let Some(note) = self
            .doc
            .boards
            .get(&board_name)
            .and_then(|ann| ann.notes.get(index))
            .cloned()
        else {
            return;
        };
        self.undo.begin_edit(&self.doc);
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
            Command::Boards => {
                // One line, tab-separated, with `*` on the board picked last: the reply protocol is one
                // line, and a tab keeps a board's own name whole even when it holds spaces.
                let mut line = String::from("ok");
                for name in self.doc.names() {
                    line.push('\t');
                    if *name == self.doc.active {
                        line.push('*');
                    }
                    line.push_str(name);
                }
                line
            }
            Command::Board(name) => {
                if self.switch_board(None, &name) {
                    "ok".into()
                } else {
                    format!("error: no board named '{name}'")
                }
            }
            Command::NewBoard(name) => {
                let Some(name) = name else {
                    let name = self.new_board(None);
                    return format!("ok {name}");
                };
                let name = name.trim().to_string();
                if name.is_empty() {
                    return "error: a board needs a name".into();
                }
                if self.doc.boards.contains_key(&name) {
                    return format!("error: a board named '{name}' already exists");
                }
                // A named board is content like an auto-named one: the file has to hold it or it is gone
                // the next time the daemon starts.
                self.undo.checkpoint(&self.doc);
                self.doc.boards.insert(name.clone(), Board::default());
                self.store.mark_dirty(Instant::now());
                self.switch_board(None, &name);
                format!("ok {name}")
            }
            Command::Share(on) => {
                self.set_shared(on);
                "ok".into()
            }
            Command::Clear => {
                self.end_text_edit();
                // `clear` empties the document, so a gesture still holding elements of it is
                // dropped: writing it would put back what this command just removed.
                self.gesture = None;
                self.cancel_transients();
                // Every board keeps its name and is emptied: a board is where a drawing lives, and `clear`
                // erases what is drawn, not the list of places to draw on. The rail goes on listing the
                // same boards, which is also what makes the command safe to run from a key binding.
                // An already-empty document has nothing to undo, so only a clear that removes
                // something is a step.
                if self
                    .doc
                    .boards
                    .values()
                    .any(|board| board != &Board::default())
                {
                    self.undo.checkpoint(&self.doc);
                }
                for board in self.doc.boards.values_mut() {
                    *board = Board::default();
                }
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
        let now = Instant::now();
        let dt = now.saturating_duration_since(self.last_tick).as_secs_f32();
        self.last_tick = now;
        self.step_zooms(dt);
        // A canvas that has stopped goes back to full resolution on its own tick, not on the next
        // pointer event: nothing may be left drawn coarse while the image is still.
        let mut settled = false;
        for out in self.outputs.values_mut() {
            let was_coarse = out.motion.quality() < 1.0;
            out.motion.idle(dt);
            if was_coarse && out.motion.quality() >= 1.0 {
                out.dirty = true;
                settled = true;
            }
        }
        self.dirty |= settled;
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
        // A caret blinks: the rail is re-snapshotted on the tick that toggles it, and the wait below
        // is what spaces those ticks out.
        if self.rename.is_some() {
            self.refresh_toolbar();
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

    /// Move every zoom in flight one frame closer to its target, and drop the ones that landed. A
    /// transition is the only thing the canvas animates, and it is driven by this tick rather than by
    /// a timer of its own, so it costs exactly the frames it is drawn on.
    fn step_zooms(&mut self, dt: f32) {
        let keys: Vec<u32> = self.outputs.keys().copied().collect();
        for key in keys {
            let stepped = {
                let Some(out) = self.outputs.get_mut(&key) else {
                    continue;
                };
                let Some(mut zoom) = out.zoom else {
                    continue;
                };
                let running = zoom.step(&mut out.view, dt);
                out.zoom = running.then_some(zoom);
                true
            };
            if stepped {
                self.view_moved(key);
            }
        }
    }

    /// The wait until the loop is worth running again: a frame while a pop-out is opening, and
    /// the idle wait otherwise.
    fn next_wait(&self) -> Duration {
        if self.animating() {
            FRAME_WAIT
        } else if let Some(rename) = &self.rename {
            // The loop wakes when the caret changes, not sixty times a second.
            let half = CARET_BLINK.as_millis();
            let left = half - rename.since.elapsed().as_millis() % half;
            Duration::from_millis(left as u64).min(IDLE_WAIT)
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
        // Some compositor updates briefly omit the name; do not regress a known board_name to the
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
        // Whether the board moved with the name: the rail marks the board its output shows, so the panel has
        // to be rebuilt for the new one.
        let mut moved_the_rail = false;
        if name_changed {
            // Finish under the old board_name before changing the name used by the document.
            self.end_text_edit();
            self.invalidate_text_caches();
        }
        self.adopt_scale(&key, output_scale(info.as_ref()));
        // The name a surface was made for is not always there when it is made — a `wl_output` burst can
        // announce the output before it names it — so the board is adopted again here, through the same
        // rule, and only while nobody has picked one: that is what tells a version 3 file's `HEADLESS-1`
        // bucket from the same screen's drawing, without overriding a board picked last.
        let moved = match self.outputs.get(&key) {
            Some(out) if out.name != name => self.doc.readopt(&name, out.board_picked, &out.board),
            _ => None,
        };
        let Some(out) = self.outputs.get_mut(&key) else {
            return;
        };
        if out.name != name {
            out.name = name;
            if let Some(board) = moved {
                out.board = board;
                out.cache.invalidate();
                moved_the_rail = true;
            }
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
        // The rail marks the board its output shows, so a board that moved with the name is a new panel.
        if moved_the_rail {
            self.refresh_toolbar();
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
        let board_name = out.board();
        let empty = Board::default();
        let ann = self.doc.boards.get(board_name).unwrap_or(&empty);
        // The accent outline and the anchor dots are overlay content too: a snap that ends must
        // damage them so the previous frame's highlight is painted over.
        let transient = match snap_hint_bounds(ann, &out.overlay) {
            Some(hint) => {
                let hint = view.screen_rect(hint);
                Some(transient.map_or(hint, |box_| box_.union(hint)))
            }
            None => transient,
        };
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
        // The move button exists exactly while a selection does, so its box joins the transient
        // damage and the frame that removes it paints over the capsule.
        let transient = if out.overlay.selection.is_some() {
            let button = move_button_bounds((w as f32, h as f32));
            Some(transient.map_or(button, |box_| box_.union(button)))
        } else {
            transient
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
        skip.extend(out.overlay.connectors.iter().map(LiveConnector::id));
        skip.extend(out.overlay.attached_texts.iter().map(|text| text.id));
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
            if out.toolbar_dirty
                && let Some(toolbar) = out.overlay.toolbar.as_ref()
            {
                box_ = box_.union(toolbar_bounds(toolbar, w as f32, h as f32));
            }
            Some(box_)
        } else if let Some(text_damage) = text_damage {
            Some(text_damage)
        } else if out.transient_dirty || out.toolbar_dirty {
            // The rail's box is fixed while editing, so the previous and the current frame agree
            // on it and there is nothing to remember between frames.
            let rail = out.toolbar_dirty.then_some(()).and(
                out.overlay
                    .toolbar
                    .as_ref()
                    .map(|toolbar| toolbar_bounds(toolbar, w as f32, h as f32)),
            );
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
                out.quality(),
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

/// Whether any output still owes frames to a zoom that has not landed. This is what makes the loop
/// come back for the next frame of the transition instead of waiting for the next input event, and it
/// is why a zoom needs no timer of its own.
fn zoom_in_flight(zooms: impl Iterator<Item = Option<Zoom>>) -> bool {
    zooms.flatten().next().is_some()
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
    crate::log::write(msg);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A caret is on for one blink and off for the next, measured from the edit's start.
    #[test]
    fn the_caret_blinks_from_the_start_of_the_edit() {
        assert!(caret_visible(Duration::ZERO));
        assert!(caret_visible(CARET_BLINK - Duration::from_millis(1)));
        assert!(!caret_visible(CARET_BLINK));
        assert!(!caret_visible(CARET_BLINK * 2 - Duration::from_millis(1)));
        assert!(caret_visible(CARET_BLINK * 2));
    }

    /// The loop comes back for a zoom's next frame only while one is in flight: this predicate is
    /// what `animating` asks, and `next_wait` is what turns it into the 16 ms wait. A zoom that has
    /// landed — or an output that never had one — must leave the loop idle rather than spinning.
    #[test]
    fn only_a_zoom_in_flight_keeps_the_loop_coming_back() {
        let view = View::default();
        let zoom = Zoom::request(view, view.zoom, [0.0, 0.0], [100.0, 100.0], 1.5);
        assert!(zoom_in_flight([None, Some(zoom), None].into_iter()));
        assert!(!zoom_in_flight([None, None].into_iter()));
        assert!(!zoom_in_flight(std::iter::empty()));
    }
}
