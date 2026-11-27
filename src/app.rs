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
    Attachment, Backdrop, Board, BoardEdit, BoardRow, Doc, ERASER_THRESHOLD, Gesture, Grab, Handle,
    ItemId, LinearHandle, LiveConnector, Motion, Note, Overlay, PEN_WIDTH_MAX, PEN_WIDTH_MIN,
    PEN_WIDTH_STEP, Palette, Press, Rect, SNAP_RADIUS, Search, Selection, Shape, ShapeKind,
    TEXT_SIZE_MAX, TEXT_SIZE_MIN, TEXT_SIZE_STEP, Target, TextBuffer, TextItem, TextOverlay,
    Toolbar, Transform, View, ZOOM_STEP, Zoom, date_stamp, link_source, marquee_select, note_drag,
    nudge_delta, selected_linear, selection_bounds, selection_frame, selection_frame_with,
    selection_press_linear, snap_square, stepped_size,
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
    DamageRegion, FrameCache, Renderer, Screen, action_card_bounds, buffer_format, cards_bounds,
    color_strip_bounds, snap_hint_bounds, toolbar_bounds, transient_bounds, transient_damage,
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
/// The interval and travel that turn two select presses on the same text box into an edit.
const TEXT_DOUBLE_CLICK: Duration = Duration::from_millis(400);
const TEXT_DOUBLE_CLICK_SLOP: f32 = 6.0;

/// The previous select press on a text box or linear element, kept only to recognise a quick second
/// one. The index names the box or shape on the board that was showing at the time.
#[derive(Clone, Copy)]
struct TextPress {
    at: Instant,
    point: [f32; 2],
    index: usize,
}

/// The vertex a pointer is dragging on a linear element. A midpoint press inserts the vertex first,
/// so the same state serves both an existing point and the new one.
#[derive(Clone, Copy)]
pub(crate) struct LinearDrag {
    pub(crate) key: u32,
    shape: usize,
    vertex: usize,
    /// For an endpoint, which of `Shape::attach` the drag may detach. Interior vertices have none.
    end: Option<usize>,
    /// A midpoint press waits until the pointer actually moves before inserting. A plain click on a
    /// line must select it, not leave a vertex behind or make the line look like it entered edit mode.
    pending: Option<usize>,
    origin: [f32; 2],
}

/// Where a clipboard read that finished off the event loop should land: in the text box being
/// edited, or as a new text box on the current board.
#[derive(Debug)]
pub(crate) enum PastedText {
    Edit(String),
    Canvas(String),
}

/// Whether `previous` and a press now on `index` at `point` are a double click rather than two
/// separate selections. The same box, a short interval and little travel, so a click followed by a
/// drag toward another box does not open the first one for writing.
fn is_text_double_click(
    previous: Option<TextPress>,
    index: usize,
    point: [f32; 2],
    now: Instant,
) -> bool {
    previous.is_some_and(|previous| {
        previous.index == index
            && now.saturating_duration_since(previous.at) <= TEXT_DOUBLE_CLICK
            && (point[0] - previous.point[0]).hypot(point[1] - previous.point[1])
                <= TEXT_DOUBLE_CLICK_SLOP
    })
}
/// How long the caret stays on, and then off. Slow enough not to pull the eye off the name.
const CARET_BLINK: Duration = Duration::from_millis(530);
const FRAME_WAIT: Duration = Duration::from_millis(16);
/// How long a shortcut-opened colour strip stays after a colour key, matching the dwell common
/// palette popovers use before they collapse again.
const COLOR_POPUP_DWELL: Duration = Duration::from_millis(1500);
pub const IDLE_WAIT: Duration = Duration::from_millis(200);
/// How long the daemon counts its own frames before deciding whether it is drawing too much.
const FRAME_WINDOW: Duration = Duration::from_secs(10);
/// Above this many unasked frames a second, the log says so. Typing and hovering a rail block redraw
/// a few times a second at most, so a window averaging more than this is a spin rather than a hand.
const BUSY_FRAMES_PER_SEC: f32 = 10.0;

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
    /// Open the AI mind-map panel on the focused screen.
    Ai,
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
            "ai" => Ok(Command::Ai),
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
    /// What the AI panel changed this frame: its whole card when any of it changed, and only the
    /// transcript's window when the wheel moved it. A scroll would otherwise re-upload a card that is
    /// mostly unchanged, sixty times a second.
    ai_damage: Option<Rect>,
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
    /// Document snapshots taken before each user-visible change, newest last, plus the ones undo
    /// left behind for redo. In memory only: a restart begins without a history.
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
    pub(crate) paste_tx: mpsc::Sender<PastedText>,
    pub(crate) pasted: mpsc::Receiver<PastedText>,
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
    /// Whether new lines and arrows use rounded corners. Session state, like the active tool.
    pub(crate) roundness: bool,
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
    /// First board in the panel's fixed-height window. The list is unbounded; the wheel moves it.
    pub(crate) board_scroll: usize,
    /// Whether the rail's width list is open. The readout toggles it; one at a time like the others.
    pub(crate) size_open: bool,

    /// What a pick in the board list applies instead of switching the screen.
    pub(crate) target: Option<Target>,
    /// Which swatch is showing its other choices, if either. The two swatches are one strip at a time,
    /// the way the panels are one panel at a time.
    pub(crate) open_palette: Option<Palette>,
    /// When a shortcut-opened colour strip closes itself; `None` while the pointer keeps it open.
    palette_until: Option<Instant>,
    /// The inside colour new closed shapes take, as a palette index. `None` is no fill, which is where a
    /// run starts: the rail's fill swatch is the only way to give a shape an inside.
    pub(crate) fill_idx: Option<usize>,
    /// The keyword search, while its card is open. `None` is closed: the drawer's row and `/` open it.
    pub(crate) search: Option<Search>,
    /// The last select press on a text box, for the quick second press that starts editing.
    last_text_press: Option<TextPress>,
    /// The last select press on a bindable container or linear element, for the quick second press
    /// that opens its bound label.
    last_container_press: Option<TextPress>,
    /// The linear vertex under the pointer, if a press is dragging one. The document is updated as
    /// the pointer moves; the gesture's undo snapshot is taken on the press.
    pub(crate) linear_drag: Option<LinearDrag>,
    /// When the colour strip started unfurling.
    palette_since: Option<Instant>,

    /// The board's name being edited in the list, if any. A name is typed where it is read, so this is
    /// app-level state and not an output's.
    pub(crate) rename: Option<Rename>,
    /// The board whose row is asking whether to delete it. The name survives a list re-sort; it is
    /// cleared when the panel closes, a pick is made, or `no` is chosen.
    pub(crate) delete_confirm: Option<String>,
    /// The AI mind-map panel: its transcript, the request in flight and the preview. Memory only: the
    /// diagram it commits is ordinary board content, the conversation is not content at all.
    pub(crate) ai: crate::ai::Chat,
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
    /// The scroll source last announced: the protocol sends it as its own event before the
    /// displacement, so a displacement that carries none belongs to this one. A wheel accelerates and
    /// a touchpad does not, and the two are told apart nowhere else.
    pub(crate) axis_source: Option<wayland_client::protocol::wl_pointer::AxisSource>,
    /// When the previous tick ran, which is the `dt` a zoom in flight is stepped by.
    last_tick: Instant,
    /// Frames drawn since `frame_window` opened, and when it did. A daemon that draws while nothing
    /// asks it to is a defect nobody notices until the fan starts, so it says so in its log.
    frames_drawn: u32,
    frame_window: Instant,
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

/// The document's undo history: snapshots taken before user-visible changes to [`Doc`], and the
/// snapshots undo left behind so they can be redone.
///
/// The snapshot is always taken *before* the mutation. A gesture and a text edit keep their
/// snapshot aside until they land, so one drag or one typing session is one undo step. Undo moves
/// the current document onto the redo stack; redo moves it back; a new change clears redo. Neither
/// direction records a change of its own, which is what keeps undo and redo out of the history.
#[derive(Debug, Default)]
pub struct UndoHistory {
    snapshots: VecDeque<Doc>,
    redo_snapshots: VecDeque<Doc>,
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

    /// Whether [`Self::undo`] has a snapshot to restore.
    pub fn can_undo(&self) -> bool {
        !self.snapshots.is_empty()
    }

    /// Number of changes that can be redone.
    pub fn redo_len(&self) -> usize {
        self.redo_snapshots.len()
    }

    /// Whether [`Self::redo`] has a snapshot to restore.
    pub fn can_redo(&self) -> bool {
        !self.redo_snapshots.is_empty()
    }

    fn push(&mut self, snapshot: Doc) {
        self.snapshots.push_back(snapshot);
        while self.snapshots.len() > UNDO_LIMIT {
            self.snapshots.pop_front();
        }
    }

    /// A new change: the redo branch no longer describes this document, so it is dropped.
    fn push_change(&mut self, snapshot: Doc) {
        self.redo_snapshots.clear();
        self.push(snapshot);
    }

    /// Keep the document as it is before a change that is known to happen.
    pub fn checkpoint(&mut self, doc: &Doc) {
        self.push_change(doc.clone());
    }

    /// Keep `before` only if the document actually changed: a shift-click or a recolour of
    /// nothing must not leave an undo step behind.
    pub fn record(&mut self, before: Doc, after: &Doc) {
        if before != *after {
            self.push_change(before);
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

    /// Take out the newest snapshot, for restoring, and keep `current` for redo. This adds no
    /// undo step: undo is not itself a change to the document. Any pending gesture or edit
    /// snapshot is dropped, because the document they described no longer exists.
    pub fn undo(&mut self, current: &Doc) -> Option<Doc> {
        self.gesture = None;
        self.edit = None;
        let snapshot = self.snapshots.pop_back()?;
        self.redo_snapshots.push_back(current.clone());
        while self.redo_snapshots.len() > UNDO_LIMIT {
            self.redo_snapshots.pop_front();
        }
        Some(snapshot)
    }

    /// Take out the newest redo snapshot, for restoring, and keep `current` as an undo step again.
    /// Like undo, this records no change of its own, and it leaves the rest of the redo stack in
    /// place so several undos can be redone one at a time.
    pub fn redo(&mut self, current: &Doc) -> Option<Doc> {
        self.gesture = None;
        self.edit = None;
        let snapshot = self.redo_snapshots.pop_back()?;
        self.push(current.clone());
        Some(snapshot)
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
            board_scroll: 0,
            size_open: false,
            target: None,
            open_palette: None,
            palette_until: None,
            palette_since: None,
            fill_idx: None,
            search: None,
            last_text_press: None,
            last_container_press: None,
            linear_drag: None,
            rename: None,
            delete_confirm: None,
            ai: crate::ai::Chat::new(),
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
            roundness: true,
            more: false,
            backdrop: Backdrop::Black,
            pan: None,
            gestures: None,
            pinch: None,
            pointer_at: None,
            pinch_state: None,
            axis_source: None,
            last_tick: Instant::now(),
            frames_drawn: 0,
            frame_window: Instant::now(),
        };
        app.bind_text_input_manager();
        app.bind_cursor_shape();
        app.bind_pointer_gestures();
        // `SeatState::new` binds the seats that already exist; it does not call `new_seat` for
        // them. The data device is what receives the compositor's selection, so bind it here as
        // well as on a hotplugged seat below.
        let seats: Vec<_> = app.seat_state.seats().collect();
        if let Some(seat) = seats.first() {
            app.data_device = Some(app.data_device_manager.get_data_device(&app.qh, seat));
        }
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
        self.ai.abort();
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
                ai_damage: None,
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
        if mode == Mode::Locked {
            // The panel is an edit-mode surface; locking is what hides it, and an answer that was
            // still arriving has nowhere to land.
            self.close_ai();
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
                    board_scroll: self.board_scroll,
                    shared: self.shared,
                    backdrop: self.backdrop,
                    zoom: self.outputs.get(&key).map_or(1.0, |out| out.view.zoom),
                    can_undo: self.undo.can_undo(),
                    can_redo: self.undo.can_redo(),
                    can_duplicate: self.outputs.get(&key).is_some_and(|out| {
                        out.overlay
                            .selection
                            .as_ref()
                            .is_some_and(|selection| !selection.items.is_empty())
                    }),
                    roundness: self.roundness,
                    fill: self.fill_idx,
                    open: self.open_palette,
                    palette_grow: hover_grow(self.palette_since, Instant::now()),
                    search: self.search.clone(),
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
            ItemId::Shape(index)
                if self
                    .doc
                    .boards
                    .get(
                        &self
                            .outputs
                            .get(&key)
                            .map(|out| out.board().to_string())
                            .unwrap_or_default(),
                    )
                    .and_then(|ann| ann.shapes.get(index))
                    .is_some_and(Shape::is_linear) =>
            {
                self.set_linear_edit(key, true);
            }
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

    /// Toggle the rounded-edge style for new linear elements and for any selected ones. With no
    /// selection this only changes the next line or arrow; with a selection it is one undoable edit.
    pub(crate) fn toggle_roundness(&mut self, acting: Option<u32>) {
        self.roundness = !self.roundness;
        let wanted = self.roundness.then_some(2);
        let keys: Vec<u32> = match acting.or(self.keyboard_focus) {
            Some(key) => vec![key],
            None => self.outputs.keys().copied().collect(),
        };
        let before = self.doc.clone();
        let mut changed = false;
        for key in keys {
            let Some(out) = self.outputs.get(&key) else {
                continue;
            };
            let board_name = out.board().to_string();
            let items = out
                .overlay
                .selection
                .as_ref()
                .map(|selection| selection.items.clone())
                .unwrap_or_default();
            let damage = {
                let Some(ann) = self.doc.boards.get_mut(&board_name) else {
                    continue;
                };
                let mut damage: Option<Rect> = None;
                for item in &items {
                    let ItemId::Shape(index) = *item else {
                        continue;
                    };
                    let Some(shape) = ann.shapes.get_mut(index) else {
                        continue;
                    };
                    if !shape.is_linear() || shape.roundness == wanted {
                        continue;
                    }
                    damage = Some(match damage {
                        Some(box_) => box_.union(shape.paint_bounds()),
                        None => shape.paint_bounds(),
                    });
                    shape.roundness = wanted;
                    changed = true;
                }
                damage
            };
            if let Some(box_) = damage {
                self.doc_changed_at(key, box_);
            }
        }
        if changed {
            self.undo.record(before, &self.doc);
        }
        self.refresh_toolbar();
        self.dirty = true;
        log(if self.roundness {
            "rounded edges armed"
        } else {
            "sharp edges armed"
        });
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

    /// Dismiss every panel: what a press outside one, a press on another rail block and a mode switch do.
    /// The setters are exclusive, so only the one that changes anything repaints the rail.
    pub(crate) fn close_panels(&mut self) {
        let confirming = self.delete_confirm.take().is_some();
        self.set_more(false);
        self.set_boards(false);
        self.set_size(false);
        self.target = None;
        if confirming {
            self.refresh_toolbar();
        }
    }

    pub(crate) fn toggle_boards(&mut self, acting: Option<u32>) {
        let open = !self.boards_open;
        self.set_boards(open);
        if open {
            self.reveal_board_in_panel(acting);
        }
    }

    /// Put the board on screen inside the panel's window when the panel opens. A long list that
    /// always starts at the top would hide the board the current output is showing.
    fn reveal_board_in_panel(&mut self, acting: Option<u32>) {
        let key = acting
            .or(self.keyboard_focus)
            .or_else(|| self.outputs.keys().next().copied());
        let Some(out) = key.and_then(|key| self.outputs.get(&key)) else {
            return;
        };
        let Some(index) = self.doc.names().position(|name| name == &out.board) else {
            return;
        };
        let visible = crate::render::board_visible_rows(out.height as f32);
        if visible == 0 {
            return;
        }
        let max = self.doc.boards.len().saturating_sub(visible);
        let next = index.saturating_sub(visible / 2).min(max);
        if next == self.board_scroll {
            return;
        }
        self.board_scroll = next;
        self.refresh_toolbar();
    }

    /// Open or close the width list. Unlike `set_boards` it keeps the hover: clearing it would make the
    /// frame that changed the list forget the block the pointer is on.
    pub(crate) fn set_size(&mut self, open: bool) {
        if self.size_open == open {
            return;
        }
        self.boards_open = false;
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
        // The panel shows the board its screen is on, so a switch changes which conversation is on
        // screen — never what that conversation may write. A request that belonged to the board being
        // left stops before the board changes: its answer is not content anywhere.
        if self.ai.open
            && self.ai.output.is_some_and(|key| {
                keys.contains(&key)
                    && self
                        .outputs
                        .get(&key)
                        .is_some_and(|out| out.board() != name)
            })
        {
            self.follow_board(name);
        }
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
        let active_changed = self.doc.active != name;
        self.doc.active = name.to_string();
        if active_changed {
            // The picked board is what a screen connected after a restart shows, so a switch is
            // content even though the drawings themselves did not change.
            self.store.mark_dirty(Instant::now());
        }
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

    /// Where a connector or pen end at a canvas point should land: a closed shape (with the
    /// attachment that follows it), a freehand/open-line polyline, or a grid intersection when the
    /// grid or dot backdrop is visible. The magnetic radius is an interface length, so it is divided
    /// by the zoom; the grid step is already a canvas length.
    pub(crate) fn snap_connector(
        &self,
        key: &u32,
        point: [f32; 2],
    ) -> Option<(Option<Attachment>, [f32; 2])> {
        let out = self.outputs.get(key)?;
        let ann = self.doc.boards.get(out.board())?;
        let grid = match self.backdrop {
            Backdrop::Grid => Some(crate::canvas::GridSnap {
                step: crate::canvas::GRID_SPACING,
                offset: [0.0, 0.0],
            }),
            Backdrop::Dots => Some(crate::canvas::GridSnap {
                step: crate::canvas::GRID_SPACING,
                offset: [crate::canvas::GRID_SPACING / 2.0; 2],
            }),
            _ => None,
        };
        ann.snap_drawing(point, SNAP_RADIUS / out.view.zoom, grid)
            .map(|snap| (snap.attachment, snap.point))
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
            self.reveal_board_in_panel(acting);
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

    /// Clone the held elements with an offset and hand the selection to the clones. `Ctrl+D` uses
    /// this directly: the document is the only clipboard for elements now, so duplicating cannot
    /// compete with the compositor's text selection.
    pub(crate) fn duplicate_held(&mut self) {
        if let Some(key) = self.keyboard_focus {
            self.duplicate_on(key);
        }
    }

    /// Duplicate the selection on one output. The card routes its click here as well as `Ctrl+D`.
    pub(crate) fn duplicate_on(&mut self, key: u32) {
        let Some((board_name, items)) = self.held(key) else {
            log("duplicate needs a selection");
            return;
        };
        let before = self.doc.clone();
        let Some((ids, box_)) = self.doc.boards.get_mut(&board_name).map(|ann| {
            let copy = ann.copied(&items).shifted(COPY_OFFSET, COPY_OFFSET);
            ann.append(&copy)
        }) else {
            return;
        };
        if ids.is_empty() {
            return;
        }
        self.undo.record(before, &self.doc);
        self.set_selection(key, ids, None);
        if let Some(box_) = box_ {
            self.doc_changed_at(key, box_);
        } else {
            self.doc_changed();
        }
        self.refresh_all_bound_labels(key);
        log("duplicated selection");
    }

    /// Ask the compositor for its current selection. There is no in-app element clipboard to take
    /// precedence, so a paste in a text box always consults Wayland.
    pub(crate) fn refresh_system_selection(&mut self) {
        if let Some(device) = self.data_device.as_ref() {
            self.selection = device.data().selection_offer();
        }
    }

    /// Put text read from the system clipboard on the current board as a new text box. Returns
    /// whether anything was inserted; an empty read is not a paste.
    pub(crate) fn paste_text(&mut self, text: String) -> bool {
        if text.is_empty() {
            return false;
        }
        let Some(key) = self.keyboard_focus else {
            log("paste needs a focused screen");
            return false;
        };
        let Some(out) = self.outputs.get(&key) else {
            return false;
        };
        let board_name = out.board.clone();
        let point = out
            .view
            .to_canvas([out.width as f32 * 0.1, out.height as f32 * 0.2]);
        let item = TextItem {
            container: None,
            x: point[0],
            y: point[1],
            color: crate::config::current().colors[self.color_idx].clone(),
            size: self.text_size,
            text,
            link: None,
        };
        let before = self.doc.clone();
        let Some(ann) = self.doc.boards.get_mut(&board_name) else {
            return false;
        };
        ann.texts.push(item.clone());
        let index = ann.texts.len() - 1;
        self.undo.record(before, &self.doc);
        self.set_selection(key, vec![ItemId::Text(index)], None);
        self.doc_changed_at(key, item.paint_bounds());
        log("pasted system text");
        true
    }

    // The AI panel. Its whole visible state is memory, and it is kept per board: one window, and one
    // conversation for each board it is used on. Opening it changes no document; the diagram a
    // conversation produces is ordinary board content and is undoable, selectable and editable there.

    /// Open the panel on `acting`'s screen — else the focused one, else the first — and show that
    /// screen's board's own conversation. The document is not touched.
    pub(crate) fn open_ai(&mut self, acting: Option<u32>) -> bool {
        self.open_ai_scope(acting, crate::ai::Scope::Board)
    }

    /// Open the conversation the selection's own card owns: its subject is what is selected, and it
    /// shares nothing with the board's conversation. Falls back to the board's when nothing is held.
    pub(crate) fn open_ai_selection(&mut self, acting: Option<u32>) -> bool {
        self.open_ai_scope(acting, crate::ai::Scope::Selection)
    }

    fn open_ai_scope(&mut self, acting: Option<u32>, scope: crate::ai::Scope) -> bool {
        if self.mode != Mode::Edit {
            self.set_mode(Mode::Edit);
        }
        let key = acting
            .or(self.keyboard_focus)
            .or_else(|| self.outputs.keys().next().copied());
        let Some(key) = key.filter(|key| self.outputs.contains_key(key)) else {
            return false;
        };
        self.end_text_edit();
        self.commit_rename();
        self.close_panels();
        if self.ai.streaming() {
            self.ai.abort();
        }
        let board = self
            .outputs
            .get(&key)
            .map(|out| out.board().to_string())
            .unwrap_or_default();
        // A selection's conversation always has something to expand; with nothing held there is no
        // subject for it, so the board's own conversation is what opens.
        let seed = self
            .ai_seed(key, &board)
            .filter(|_| scope == crate::ai::Scope::Selection);
        let scope = if seed.is_some() || scope == crate::ai::Scope::Board {
            scope
        } else {
            crate::ai::Scope::Board
        };
        self.ai.enter_scope(&board, scope);
        if scope == crate::ai::Scope::Selection {
            // The panel keeps its subject even as the selection changes under it: what the card opened
            // is what the request expands.
            self.ai.session_mut().seed = seed;
        }
        if !self.ai.open {
            // The card slides into place over the rail's own transition. Opening again while it is
            // still arriving keeps the clock it has, so the slide is not restarted mid-flight.
            self.ai.open_since = Some(Instant::now());
        }
        self.ai.open = true;
        self.ai.output = Some(key);
        self.ai.focus = true;
        self.sync_text_input();
        self.refresh_ai();
        self.dirty = true;
        true
    }

    /// The board on screen changed: switch the panel to that board's conversation, and stop an answer
    /// that belonged to the board being left. Every board keeps its own transcript, draft and diagram
    /// ids, so nothing crosses between them.
    pub(crate) fn follow_board(&mut self, board: &str) {
        if self.ai.streaming() {
            self.ai.abort();
        }
        self.ai.enter(board);
        self.sync_text_input();
        self.refresh_ai();
        self.dirty = true;
    }

    /// Hide the panel and stop the request. Every board's conversation and diagram ids stay, so a later
    /// prompt in the same session continues where it left off.
    pub(crate) fn close_ai(&mut self) {
        if !self.ai.open {
            return;
        }
        self.ai.abort();
        self.ai.clear_preview();
        self.ai.focus = false;
        self.ai.pointer_captured = false;
        self.ai.output = None;
        self.ai.open = false;
        log("AI panel closed");
        self.sync_text_input();
        self.refresh_ai();
        self.dirty = true;
    }

    /// Take the keyboard out of the panel's input line and leave the panel open: a press on the canvas
    /// goes on to draw, but the conversation stays readable.
    pub(crate) fn blur_ai(&mut self) {
        if !self.ai.focus && self.ai.session().preedit.is_empty() {
            return;
        }
        self.ai.focus = false;
        self.ai.session_mut().preedit.clear();
        self.sync_text_input();
        self.refresh_ai();
        self.dirty = true;
    }

    /// The furthest the transcript can scroll on `key`'s screen, in logical pixels.
    pub(crate) fn ai_max_scroll(&mut self, key: u32) -> f32 {
        let Some((size, toolbar)) = self.outputs.get(&key).map(|out| {
            (
                (out.width as f32, out.height as f32),
                out.overlay.toolbar.clone(),
            )
        }) else {
            return 0.0;
        };
        let layout = crate::render::ai_panel_layout(size, toolbar.as_ref());
        let view = self.ai.view();
        (self.renderer.ai_log_height(&view, layout.log.w) - layout.log.h).max(0.0)
    }

    /// Draw the shown conversation on the screen that owns the panel, and clear the panel everywhere
    /// else. One panel, on one output: the others must not keep a stale copy of it.
    pub(crate) fn refresh_ai(&mut self) {
        let Some(key) = self.ai.output else {
            for out in self.outputs.values_mut() {
                if out.overlay.ai.take().is_some() {
                    // The card is gone from the overlay, so its pixels have to be painted over: the last
                    // frame may only have damaged the transcript's window inside it.
                    let layout = crate::render::ai_panel_layout(
                        (out.width as f32, out.height as f32),
                        out.overlay.toolbar.as_ref(),
                    );
                    out.ai_damage = Some(layout.panel.grown(crate::render::AI_PANEL_SLIDE));
                    out.transient_dirty = true;
                }
                out.overlay.ai_preview = None;
            }
            self.dirty = true;
            return;
        };
        let Some((size, toolbar)) = self.outputs.get(&key).map(|out| {
            (
                (out.width as f32, out.height as f32),
                out.overlay.toolbar.clone(),
            )
        }) else {
            return;
        };
        let layout = crate::render::ai_panel_layout(size, toolbar.as_ref());
        let mut view = self.ai.view();
        let content = self.renderer.ai_log_height(&view, layout.log.w);
        let max = (content - layout.log.h).max(0.0);
        {
            let now = Instant::now();
            let session = self.ai.session_mut();
            session.clamp_scroll(max);
            // The burst is stamped here, at the moment the frame is built, not when the next tick runs:
            // a wheel notch that arrives between two frames has to show its number on the frame the
            // notch itself causes.
            let burst = session.burst.label(now);
            session.burst_drawn = burst;
            view.scroll = session.scroll;
            view.burst = burst;
        }
        view.grow = hover_grow(self.ai.open_since, Instant::now());
        let preview = self.ai.session().preview.clone();
        for (output_key, out) in self.outputs.iter_mut() {
            let had_panel = out.overlay.ai.is_some();
            let had_preview = out.overlay.ai_preview.is_some();
            if *output_key != key {
                out.overlay.ai = None;
                out.overlay.ai_preview = None;
            }
            if had_panel || had_preview {
                out.transient_dirty = true;
            }
        }
        if let Some(out) = self.outputs.get_mut(&key) {
            out.overlay.ai = Some(view);
            out.overlay.ai_preview = preview;
            out.ai_damage = Some(
                crate::render::ai_panel_layout(
                    (out.width as f32, out.height as f32),
                    out.overlay.toolbar.as_ref(),
                )
                .panel
                .grown(crate::render::AI_PANEL_SLIDE),
            );
            out.transient_dirty = true;
        }
        self.dirty = true;
    }

    /// The transcript moved and nothing else did: the view's offset and the strip's number are the only
    /// things a frame has to carry, and only inside the log's own window.
    pub(crate) fn refresh_ai_scroll(&mut self) {
        let Some(key) = self.ai.output else {
            return;
        };
        let session = self.ai.session();
        let (scroll, burst) = (session.scroll, session.burst_drawn);
        let Some(out) = self.outputs.get_mut(&key) else {
            return;
        };
        let Some(view) = out.overlay.ai.as_mut() else {
            return;
        };
        if view.scroll == scroll && view.burst == burst {
            return;
        }
        view.scroll = scroll;
        view.burst = burst;
        let layout = crate::render::ai_panel_layout(
            (out.width as f32, out.height as f32),
            out.overlay.toolbar.as_ref(),
        );
        out.ai_damage = Some(layout.log.grown(1.0));
        out.transient_dirty = true;
        self.dirty = true;
    }

    /// What `board` holds under the selection tool, as a request to expand it. Only the board the
    /// request is for counts: a selection on another screen is not what the conversation is about.
    fn ai_seed(&self, key: u32, board: &str) -> Option<crate::canvas::Seed> {
        let out = self.outputs.get(&key)?;
        if out.board() != board {
            return None;
        }
        let selection = out.overlay.selection.as_ref()?;
        if selection.items.is_empty() {
            return None;
        }
        let board = self.doc.boards.get(out.board())?;
        crate::canvas::seed_of(board, &selection.items)
    }

    /// Send the typed prompt against the board on screen. Every refusal leaves the prompt in the input
    /// so it can be edited rather than retyped.
    ///
    /// An empty input with a selection is a request of its own: the selection is the subject, and the
    /// answer takes its place.
    pub(crate) fn send_ai(&mut self) {
        if self.ai.streaming() {
            return;
        }
        let seed = self
            .ai
            .output
            .and_then(|key| self.ai_seed(key, self.ai.board()))
            .or_else(|| self.ai.session().seed.clone());
        let typed = self.ai.session().input.text.trim().to_string();
        let prompt = if typed.is_empty() {
            if seed.is_none() {
                return;
            }
            crate::ai::EXPAND_PROMPT.to_string()
        } else {
            typed
        };
        if prompt.len() > crate::ai::MAX_PROMPT_BYTES {
            self.ai.session_mut().error = Some("prompt is too long".into());
            self.refresh_ai();
            return;
        }
        let settings = match crate::ai::resolve(crate::config::current()) {
            Ok(settings) => settings,
            Err(e) => {
                self.ai.session_mut().error = Some(e);
                self.refresh_ai();
                return;
            }
        };
        let Some(key) = self.ai.output else {
            self.cancel_ai("the AI panel has no screen");
            return;
        };
        let Some((size, toolbar, view, board)) = self.outputs.get(&key).map(|out| {
            (
                (out.width as f32, out.height as f32),
                out.overlay.toolbar.clone(),
                out.view,
                out.board().to_string(),
            )
        }) else {
            self.cancel_ai("the AI panel's screen is gone");
            return;
        };
        if !self.doc.boards.contains_key(&board) {
            self.cancel_ai("the AI panel's board is gone");
            return;
        }
        if self.ai.board() != board {
            // The screen moved to another board without the panel being told: never answer onto a board
            // this conversation does not belong to.
            self.follow_board(&board);
        }
        // The diagram goes where the panel is not: the strip the panel leaves, at the middle of the
        // output's height, in canvas coordinates. Its arrangement decides where in that room it grows.
        let layout = crate::render::ai_panel_layout(size, toolbar.as_ref());
        let strip = [(layout.panel.x - 24.0).max(48.0), size.1 * 0.5];
        let anchor = view.to_canvas([48.0, strip[1]]);
        let room = (strip[0] - 48.0).max(0.0) / view.zoom;
        let colors = &crate::config::current().colors;
        let color = colors
            .get(self.color_idx)
            .cloned()
            .unwrap_or_else(|| colors[0].clone());
        if let Some(seed) = &seed {
            // What a request expands is worth saying out loud: it is the one place a wrong selection
            // turns into a wrong diagram.
            log(&format!(
                "ai: expanding {} shape(s), {} text(s) at ({:.0}, {:.0}) on {board}",
                seed.shapes.len(),
                seed.texts.len(),
                seed.frame.x + seed.frame.w / 2.0,
                seed.frame.y + seed.frame.h / 2.0,
            ));
        }
        {
            let session = self.ai.session_mut();
            session.anchor = anchor;
            session.room = room;
            session.color = color;
            session.seed = seed;
            session.place = None;
            session.preview = None;
            session.error = None;
            session.follow = true;
            session.push(crate::ai::Role::User, prompt, false);
            session.push(crate::ai::Role::Assistant, String::new(), true);
            session.input = TextBuffer::new("");
            session.preedit.clear();
        }
        self.refresh_ai();
        if let Err(e) = self.ai.start(settings, self.ping.clone()) {
            let session = self.ai.session_mut();
            session.stop_streaming();
            session.error = Some(e);
            self.refresh_ai();
        }
    }

    /// The send button while an answer streams: stop it, keeping what arrived.
    pub(crate) fn stop_ai(&mut self) {
        self.cancel_ai("stopped");
    }

    /// Give up on the request in flight for `reason` and say so. The document is untouched: a partial
    /// answer was never content, and it belongs to the board that asked for it.
    pub(crate) fn cancel_ai(&mut self, reason: &str) {
        // The failure belongs to the conversation that asked, which is not always the one on screen.
        self.ai.fail_asking(reason);
        self.ai.abort();
        self.refresh_ai();
        self.dirty = true;
    }

    /// Whether the panel is still arriving. Its clock is the usual one, but a frame that takes longer
    /// than the whole transition — a slow or debug build — would otherwise leave the card a hair off
    /// its place forever, so a card that is not at its settled position keeps the loop running too.
    fn ai_arriving(&self) -> bool {
        let now = Instant::now();
        (self.ai.open
            && self
                .ai
                .open_since
                .is_some_and(|started| now.saturating_duration_since(started) < TOOLBAR_ANIM))
            || self.outputs.values().any(|out| {
                out.overlay
                    .ai
                    .as_ref()
                    .is_some_and(|view| (view.grow - 1.0).abs() > 0.001)
            })
    }

    /// The panel's own motion: the transcript easing toward the wheel's offset, the card sliding into
    /// place, and the burst's number leaving the strip. Nothing here touches the document.
    fn animate_ai(&mut self, now: Instant, dt: f32) {
        let sliding = self.ai_arriving();
        let moved = self.ai.step(now, dt);
        if sliding || moved.view {
            self.refresh_ai();
        } else if moved.scroll {
            self.refresh_ai_scroll();
        }
    }

    /// The stream's own tick: drain the reader, rebuild the preview when it is due, and commit the
    /// diagram once the answer is complete.
    fn pump_ai(&mut self) {
        let was = self.ai.streaming();
        if self.ai.poll() {
            self.refresh_ai();
        }
        // Rebuild only when the answer has grown since the last build. A stream that never sends its
        // end marker stays open, and a timer that re-parsed the same text would spend the machine on
        // nothing: an answer that is already on screen must not be laid out again.
        let streaming = self.ai.streaming();
        if streaming && self.ai.session().last_preview.elapsed() >= crate::ai::PREVIEW_INTERVAL {
            let answer_len = self
                .ai
                .session()
                .messages
                .last()
                .filter(|message| !message.text.is_empty())
                .map_or(0, |message| message.text.len());
            if answer_len != self.ai.session().preview_len {
                self.refresh_ai_preview();
            }
        }
        if was && !streaming {
            if self.ai.session().error.is_some() {
                // A failed or stopped request leaves nothing on the board, not even the part that parsed.
                self.ai.clear_preview();
                self.refresh_ai();
            } else {
                self.refresh_ai_preview();
                if self.ai.session().error.is_none() {
                    self.commit_ai_preview();
                }
            }
            self.ai.session_mut().follow = true;
            self.refresh_ai();
        }
    }

    /// Re-parse the answer so far and lay it out. A half-received answer that does not parse yet keeps
    /// the previous preview; a complete one that does not parse is an error and changes no document.
    fn refresh_ai_preview(&mut self) {
        let Some(last) = self.ai.session().messages.last() else {
            return;
        };
        if last.role != crate::ai::Role::Assistant {
            return;
        }
        let text = last.text.clone();
        let streaming = last.streaming;
        let failure = "AI did not return a valid Mermaid mindmap".to_string();
        let mermaid = crate::ai::extract_mermaid(&text);
        // The syntax says what is written — a mind map, a flowchart, a timeline, a quadrant chart — and the
        // structure it yields is what every arrangement is drawn from.
        let parsed = mermaid
            .as_deref()
            .map(crate::ai::parse_diagram)
            .unwrap_or_else(|| Err(failure.clone()));
        let parsed = match parsed {
            Ok(parsed) => parsed,
            Err(reason) => {
                if !streaming && self.ai.session().error.is_none() {
                    self.ai.session_mut().error = Some(
                        if reason.starts_with("the mind map")
                            || reason.starts_with("the flowchart")
                            || reason.starts_with("the timeline")
                            || reason.starts_with("the quadrant")
                        {
                            reason
                        } else {
                            failure
                        },
                    );
                    self.ai.clear_preview();
                    self.refresh_ai();
                    self.dirty = true;
                }
                return;
            }
        };
        let tree = parsed.tree;
        // The answer's own `%%` line is an instruction and pins the arrangement. A syntax that implies one
        // is a hint the rules weigh, and with neither, the shape of the tree decides: the cheapest of the
        // arrangements the rule allows is the one drawn.
        let preference = mermaid.as_deref().and_then(crate::ai::layout_directive);
        let hint = parsed.view;
        let (anchor, room, color, seed) = {
            let session = self.ai.session();
            (
                session.anchor,
                session.room,
                session.color.clone(),
                session.seed.clone(),
            )
        };
        // Maps pile up as conversations do, so an answer goes where the board has room for it: past the
        // maps already standing there, and off the words this one replaces.
        let board_name = self.ai.board().to_string();
        let taken = self.ai_taken(&board_name);
        let preferred = self.ai.session().place;
        let limit = anchor[0] + room;
        let renderer = &mut self.renderer;
        let choice = crate::ai::choose_layout(
            preference,
            hint,
            &tree,
            anchor,
            room,
            &color,
            seed.as_ref().map(|seed| seed.frame),
            &taken,
            limit,
            preferred,
            &mut |text, size, wrap| renderer.measure_text(text, size, wrap),
        );
        if !streaming {
            log(&format!(
                "ai: {:?} at ({:.0}, {:.0}), {} wide, {} crossing(s), {} covered",
                choice.kind,
                choice.place.unwrap_or_default()[0],
                choice.place.unwrap_or_default()[1],
                choice.cost.width.round(),
                choice.cost.crossings,
                choice.cost.covered.max(0.0).round(),
            ));
        }
        let answer_len = text.len();
        let session = self.ai.session_mut();
        session.preview = Some(choice.board);
        session.view = Some(choice.kind);
        session.place = choice.place;
        session.last_preview = Instant::now();
        session.preview_len = answer_len;
        self.refresh_ai();
    }

    /// Draw the last answer again in another arrangement, without asking the model anything: the Mermaid
    /// it sent is still in the transcript, so the switch is the same parse and layout as the first draw,
    /// committed over the diagram that stands there now, as one undo step.
    pub(crate) fn cycle_ai_view(&mut self) -> bool {
        if self.ai.output.is_none() {
            return false;
        }
        let board_name = self.ai.board().to_string();
        let tree = self
            .ai
            .session()
            .messages
            .iter()
            .rev()
            .find(|message| message.role == crate::ai::Role::Assistant)
            .and_then(|message| crate::ai::extract_mermaid(&message.text))
            .and_then(|mermaid| crate::ai::parse_diagram(&mermaid).ok())
            .map(|parsed| parsed.tree);
        let Some(tree) = tree else {
            return false;
        };
        let current = self.ai.session().view;
        let next = crate::ai::next_view(current, &tree);
        // The diagram it replaces decides where the new one stands: the same corner, so a switch looks
        // like the same map redrawn rather than a second map appearing.
        let old = self
            .doc
            .boards
            .get(&board_name)
            .and_then(|board| {
                let ids: Vec<crate::canvas::ItemId> = self
                    .ai
                    .session()
                    .placed_shapes
                    .iter()
                    .filter_map(|id| board.shape_index_by_id(*id))
                    .map(crate::canvas::ItemId::Shape)
                    .collect();
                crate::canvas::selection_frame(board, &ids, None)
            })
            .map(|box_| [box_.x, box_.y]);
        let (anchor, room, color) = {
            let session = self.ai.session();
            (session.anchor, session.room, session.color.clone())
        };
        let limit = anchor[0] + room;
        let taken = self.ai_taken(&board_name);
        let preferred = old.or(self.ai.session().place);
        let renderer = &mut self.renderer;
        let board = crate::ai::layout_mindmap(
            next,
            &tree,
            anchor,
            room,
            &color,
            &mut |text, size, wrap| renderer.measure_text(text, size, wrap),
        );
        let (board, place) = crate::ai::free_place(board, &taken, limit, preferred);
        {
            let session = self.ai.session_mut();
            session.preview = Some(board);
            session.view = Some(next);
            session.place = place;
            session.error = None;
        }
        log(&format!(
            "ai: view switched to {} without asking the model",
            next.name()
        ));
        self.commit_ai_preview();
        self.refresh_ai();
        self.dirty = true;
        true
    }

    /// What the board already holds, as boxes an answer may not cover. Nothing this answer replaces is in
    /// the list: the diagram this conversation placed last time, the shapes and texts its selection named,
    /// and the labels of both, all leave the board when the answer is committed.
    fn ai_taken(&self, board_name: &str) -> Vec<Rect> {
        let Some(board) = self.doc.boards.get(board_name) else {
            return Vec::new();
        };
        let session = self.ai.session();
        let mut gone: Vec<u64> = session.placed_shapes.clone();
        let mut gone_texts: Vec<usize> = Vec::new();
        if let Some(seed) = &session.seed {
            gone.extend(seed.shapes.iter().copied());
            gone_texts.extend(seed.texts.iter().map(|(index, _)| *index));
        }
        let mut taken = Vec::new();
        for shape in &board.shapes {
            if !gone.contains(&shape.id) {
                taken.push(shape.paint_bounds());
            }
        }
        for (index, text) in board.texts.iter().enumerate() {
            let is_gone = gone_texts.contains(&index)
                || text
                    .container
                    .is_some_and(|container| gone.contains(&container));
            if !is_gone {
                taken.push(text.paint_bounds());
            }
        }
        for note in &board.notes {
            taken.push(note.bounds());
        }
        for stroke in &board.strokes {
            taken.extend(stroke.paint_bounds());
        }
        taken
    }

    /// Write the preview into its board as one change: the diagram this conversation placed last time
    /// is removed, the new one appended, and the whole replacement is one undo step. The board is the
    /// one the conversation was started on, never the one that happens to be on screen now.
    fn commit_ai_preview(&mut self) {
        let Some(preview) = self.ai.session_mut().preview.take() else {
            return;
        };
        let board_name = self.ai.board().to_string();
        let before = self.doc.clone();
        let Some(board) = self.doc.boards.get_mut(&board_name) else {
            self.ai.session_mut().error =
                Some(format!("the AI diagram's board '{board_name}' is gone"));
            self.refresh_ai();
            return;
        };
        let mut shapes_to_remove = Vec::new();
        for id in self.ai.session().placed_shapes.clone() {
            if let Some(index) = board.shape_index_by_id(id) {
                shapes_to_remove.push(ItemId::Shape(index));
            }
        }
        // The words this request was asked to expand give up their place to the answer. A text is
        // removed only while it still says what was selected, so an edit made while the answer was
        // arriving is never cut into.
        let seed = self.ai.session().seed.clone();
        if let Some(seed) = &seed {
            for id in &seed.shapes {
                if let Some(index) = board.shape_index_by_id(*id) {
                    shapes_to_remove.push(ItemId::Shape(index));
                }
            }
            for (index, text) in &seed.texts {
                if board
                    .texts
                    .get(*index)
                    .is_some_and(|held| &held.text == text)
                {
                    shapes_to_remove.push(ItemId::Text(*index));
                }
            }
        }
        let removed = board.remove(&shapes_to_remove);
        let (appended, added) = board.append(&preview);
        let synced = board.sync_connectors();
        // The commit centres every label on its node with the renderer's own measurement, so the
        // committed text sits exactly where the preview drew it.
        let labels: Vec<usize> = appended
            .iter()
            .filter_map(|id| match id {
                ItemId::Text(index) => Some(*index),
                _ => None,
            })
            .collect();
        for index in labels {
            let Some(item) = board.texts.get(index).cloned() else {
                continue;
            };
            let Some(container) = item.container else {
                continue;
            };
            let Some(shape) = board
                .shapes
                .iter()
                .find(|shape| shape.id == container)
                .cloned()
            else {
                continue;
            };
            let placed = self.renderer.place_bound_text(&item, &shape);
            if let Some(text) = board.texts.get_mut(index) {
                *text = placed;
            }
        }
        let placed_shapes: Vec<u64> = appended
            .iter()
            .filter_map(|id| match id {
                ItemId::Shape(index) => board.shapes.get(*index).map(|shape| shape.id),
                _ => None,
            })
            .collect();
        let count = placed_shapes.len();
        self.ai.session_mut().placed_shapes = placed_shapes;
        // The answer is what the user wants next: it is selected on every screen showing its board, so a
        // move, a colour or a delete acts on what the request just made. A label is not in the list: it
        // rides with the shape it is bound to, and a selected text is a text a resize would re-size.
        let selected: Vec<ItemId> = appended
            .iter()
            .copied()
            .filter(|id| match id {
                ItemId::Text(index) => board
                    .texts
                    .get(*index)
                    .is_some_and(|text| text.container.is_none()),
                _ => true,
            })
            .collect();
        let shown: Vec<u32> = self
            .outputs
            .iter()
            .filter(|(_, out)| out.board() == board_name)
            .map(|(key, _)| *key)
            .collect();
        if !shown.is_empty() && self.tool != Tool::Selection {
            self.set_tool(Tool::Selection);
        }
        for key in shown {
            if let Some(out) = self.outputs.get_mut(&key) {
                out.overlay.selection = Some(Selection {
                    items: selected.clone(),
                    transform: None,
                });
                out.transient_dirty = true;
            }
        }
        let mut box_: Option<Rect> = None;
        for part in [removed, added, synced].into_iter().flatten() {
            box_ = Some(box_.map_or(part, |box_| box_.union(part)));
        }
        self.undo.record(before, &self.doc);
        match (self.ai.output, box_) {
            (Some(key), Some(box_)) if self.outputs.contains_key(&key) => {
                self.doc_changed_at(key, box_)
            }
            _ => self.doc_changed(),
        }
        self.ai.clear_preview();
        self.ai.session_mut().follow = true;
        self.refresh_ai();
        log(&format!(
            "AI diagram committed on {board_name}: {count} shapes"
        ));
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
        // The conversation went with the board, like everything else it held.
        self.ai.forget(name);
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
        let now = Instant::now();
        self.toolbar_hover_since
            .is_some_and(|started| now.saturating_duration_since(started) < TOOLBAR_ANIM)
            || self
                .panel_open_since
                .is_some_and(|started| now.saturating_duration_since(started) < TOOLBAR_ANIM)
            || self
                .palette_since
                .is_some_and(|started| now.saturating_duration_since(started) < TOOLBAR_ANIM)
            || zoom_in_flight(self.outputs.values().map(|out| out.zoom))
            || self.outputs.values().any(|out| out.motion.quality() < 1.0)
            || self.ai_arriving()
            || self.ai.scrolling()
    }

    /// The toolbar control under a pointer position on an output, if any.
    pub(crate) fn toolbar_hit(&mut self, key: &u32, x: f32, y: f32) -> Option<ToolbarAction> {
        let out = self.outputs.get(key)?;
        let toolbar = out.overlay.toolbar.clone()?;
        let (width, height) = (out.width as f32, out.height as f32);
        self.renderer.toolbar_hit(&toolbar, width, height, x, y)
    }

    /// Move the boards panel's window by `steps` rows. The panel has a fixed height, so this is the
    /// only way to reach a board past the visible rows; the document index in each row does not move.
    pub(crate) fn scroll_board_panel(&mut self, key: &u32, steps: i32) {
        let Some(out) = self.outputs.get(key) else {
            return;
        };
        let visible = crate::render::board_visible_rows(out.height as f32);
        if visible == 0 {
            return;
        }
        let max = self.doc.boards.len().saturating_sub(visible);
        let next = (self.board_scroll as i64 + i64::from(steps)).clamp(0, max as i64) as usize;
        if next == self.board_scroll {
            return;
        }
        self.board_scroll = next;
        self.refresh_toolbar();
    }

    /// Which swatch's open card a pointer position sits on, gaps included.
    pub(crate) fn toolbar_over_palette(&mut self, key: &u32, x: f32, y: f32) -> Option<Palette> {
        let out = self.outputs.get(key)?;
        let toolbar = out.overlay.toolbar.clone()?;
        let (width, height) = (out.width as f32, out.height as f32);
        self.renderer
            .toolbar_over_palette(&toolbar, width, height, x, y)
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

    /// Hand the pointer back to `select` once a drawing tool has produced something. A drawing is a
    /// one-shot act: the next gesture is a pick or a move, not another accidental mark. Tool and
    /// panel state are not document changes, so this never creates an undo step.
    pub(crate) fn select_after_drawing(&mut self) {
        if !self.tool.hands_to_select() {
            return;
        }
        self.tool = Tool::Selection;
        log("tool switched to Selection after drawing");
        self.refresh_toolbar();
        self.sync_cursor();
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
        self.refresh_history_cards();
    }

    /// Flip the history cards' enabled state without rebuilding the rail's board list and panels on
    /// every stroke. The rail's shape does not change with the undo stacks; only these two flags do.
    fn refresh_history_cards(&mut self) {
        let (can_undo, can_redo) = (self.undo.can_undo(), self.undo.can_redo());
        for out in self.outputs.values_mut() {
            let Some(toolbar) = out.overlay.toolbar.as_mut() else {
                continue;
            };
            if toolbar.can_undo != can_undo || toolbar.can_redo != can_redo {
                toolbar.can_undo = can_undo;
                toolbar.can_redo = can_redo;
                out.toolbar_dirty = true;
            }
        }
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
    /// file and the screen have to agree with the document again. The document being left is kept
    /// for [`Self::redo`], but no undo step is added: undo is not itself a change in the history.
    pub(crate) fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.undo(&self.doc) else {
            return false;
        };
        self.restore_document(previous);
        log("undo");
        true
    }

    /// Restore the newest redo snapshot. Returns `false` when there is nothing to redo, which
    /// changes nothing — not even a dirty flag or a repaint. Like undo, this takes no new history
    /// step: the snapshot returns to the undo stack, where it came from.
    pub(crate) fn redo(&mut self) -> bool {
        let Some(next) = self.undo.redo(&self.doc) else {
            return false;
        };
        self.restore_document(next);
        log("redo");
        true
    }

    /// Put a document snapshot back on screen and make everything that names the replaced
    /// document's contents agree with it.
    fn restore_document(&mut self, doc: Doc) {
        self.gesture = None;
        self.selection_grab = None;
        self.linear_drag = None;
        self.last_container_press = None;
        self.pending_duplicate = None;
        self.pending_links.clear();
        self.note_origin = None;
        self.note_sized = false;
        self.delete_confirm = None;
        self.rename = None;
        self.im_preedit = None;
        self.im_commit.clear();
        self.im_delete = None;
        self.doc = doc;
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
                // The fallback is where the restore put this screen; a late output name must not
                // move it.
                out.board_picked = true;
            }
        }
        self.doc_changed();
        self.refresh_toolbar();
        self.sync_text_input();
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
        self.linear_drag = None;
        self.last_container_press = None;
        self.pending_links.clear();
        let Some(out) = self.outputs.get_mut(&key) else {
            return;
        };
        let changed = out.overlay.stroke.take().is_some()
            || out.overlay.shape.take().is_some()
            || out.overlay.eraser.take().is_some()
            || out.overlay.note.take().is_some()
            || out.overlay.selection.take().is_some()
            || out.overlay.linear_edit.take().is_some()
            || out.overlay.linear_drag.take().is_some()
            || out.overlay.marquee.take().is_some();
        out.overlay.connectors.clear();
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
        let (press, frame, at, was_editing) = {
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
            let linear_edit = out.overlay.linear_edit.is_some();
            let press = selection_press_linear(
                ann,
                &held,
                view,
                [x, y],
                view.canvas_len(ERASER_THRESHOLD),
                self.shift,
                &boxes,
                linear_edit,
                out.overlay.linear_drag,
            );
            let frame = selection_frame_with(ann, &held, None, &boxes);
            (press, frame, view.to_canvas([x, y]), linear_edit)
        };
        if was_editing && !matches!(press, Press::Linear(_)) {
            self.set_linear_edit(key, false);
        }
        let alt = self.alt;
        match press {
            Press::Linear(handle) => self.begin_linear_handle(key, handle),
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
                // The element under the pointer, and only that one: a shape is not a group of what
                // happens to be drawn inside it. Grouping is what a marquee is for.
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
                let items = crate::canvas::toggle_selection(&held, id);
                self.set_selection(key, items, None);
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
        // is part of the transform the frame draws, so the shape visibly lands on the end.
        let old_links = std::mem::take(&mut self.pending_links);
        if matches!(gesture, Gesture::Move { .. }) && !transform.is_identity() {
            let items = self
                .outputs
                .get(&key)
                .and_then(|out| out.overlay.selection.as_ref())
                .map(|selection| selection.items.clone())
                .unwrap_or_default();
            let (snapped, links) = self.snap_moved_shape(&key, &items, transform);
            transform = snapped;
            self.pending_links = links;
        } else {
            self.pending_links.clear();
        }
        let links_changed = old_links != self.pending_links;
        let next =
            (!transform.is_identity() || !self.pending_links.is_empty()).then_some(transform);
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
        if changed || links_changed {
            self.refresh_live_connectors(key);
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

    /// Rebuild the attached-connector copies the overlay paints during a selection drag, and skip
    /// their committed geometry in the raster.
    fn refresh_live_connectors(&mut self, key: u32) {
        let Some((board_name, items, transform)) = self.outputs.get(&key).and_then(|out| {
            let selection = out.overlay.selection.as_ref()?;
            let transform = selection.transform?;
            Some((out.board().to_string(), selection.items.clone(), transform))
        }) else {
            if let Some(out) = self.outputs.get_mut(&key) {
                out.overlay.connectors.clear();
            }
            return;
        };
        let pending: Vec<_> = self
            .pending_links
            .iter()
            .map(|link| (link.connector, link.end, link.attachment))
            .collect();
        let connectors = self
            .doc
            .boards
            .get(&board_name)
            .map(|board| board.connector_preview(&items, transform, &pending))
            .unwrap_or_default();
        if let Some(out) = self.outputs.get_mut(&key) {
            out.overlay.connectors = connectors;
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
        let before = self.outputs.get(&key).map(|out| out.view.zoom);
        if let Some(out) = self.outputs.get_mut(&key) {
            f(&mut out.view);
        }
        self.view_moved(key);
        if self.outputs.get(&key).map(|out| out.view.zoom) != before {
            self.refresh_toolbar();
        }
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
        self.linear_drag = None;
        self.last_container_press = None;
        self.undo.abandon_gesture();
        let mut dropped = false;
        for out in self.outputs.values_mut() {
            let had = out.overlay.selection.take().is_some()
                | out.overlay.marquee.take().is_some()
                | out.overlay.linear_edit.take().is_some()
                | out.overlay.linear_drag.take().is_some();
            out.overlay.connectors.clear();
            if had {
                out.transient_dirty = true;
                dropped = true;
            }
        }
        if dropped {
            self.gesture = None;
            self.dirty = true;
            self.refresh_toolbar();
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
        if let Some(drag) = self.linear_drag {
            self.release_linear_drag(drag.key);
        }
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
        let owned =
            (!items.is_empty() || transform.is_some()).then_some(Selection { items, transform });
        let mut changed = false;
        for (k, out) in self.outputs.iter_mut() {
            let next = if *k == key { owned.clone() } else { None };
            if out.overlay.selection != next {
                out.overlay.selection = next;
                // Point editing names an index in the selection that just changed.
                out.overlay.linear_edit = None;
                out.overlay.linear_drag = None;
                out.overlay.connectors.clear();
                out.transient_dirty = true;
                self.dirty = true;
                changed = true;
            }
        }
        if changed {
            // The cards' enabled states are part of the rail the frame paints.
            self.refresh_toolbar();
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
        let Some(transform) = selection.transform else {
            return;
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
            // A moved shape drags every connector attached to it, whether or not the connector was
            // part of the selection. Both endpoints are re-seated on their shapes here, once, on the
            // same commit as the move.
            if let Some(connector_box) = ann.sync_connectors() {
                box_ = Some(box_.map_or(connector_box, |box_| box_.union(connector_box)));
            }
        }
        // A container's label follows the container's new geometry, including a connector whose end
        // an attached shape just moved. It is a second document change on the same gesture, so it
        // stays under the same undo snapshot.
        self.refresh_all_bound_labels(key);
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

    /// Show the other stroke colours after a colour shortcut. The active swatch on the rail has already
    /// changed; this only opens the strip for a short dwell.
    pub(crate) fn reveal_colors(&mut self) {
        if self.open_palette != Some(Palette::Stroke) {
            self.palette_since = Some(Instant::now());
        }
        self.open_palette = Some(Palette::Stroke);
        self.palette_until = Some(Instant::now() + COLOR_POPUP_DWELL);
        self.refresh_toolbar();
    }

    /// Hold the strip while the pointer is on the swatch that opened it or any of its colours; once the
    /// pointer leaves a strip with no shortcut deadline, it collapses. Moving from one swatch to the
    /// other switches the strip rather than closing it, so the two are one control with two faces.
    pub(crate) fn hover_palette(&mut self, over: Option<Palette>) {
        match over {
            Some(which) if self.open_palette != Some(which) => {
                self.palette_since = Some(Instant::now());
                self.open_palette = Some(which);
                self.palette_until = None;
                self.refresh_toolbar();
            }
            Some(_) if self.palette_until.is_some() => {
                self.palette_until = None;
                self.refresh_toolbar();
            }
            Some(_) => {}
            None if self.palette_until.is_none() && self.open_palette.is_some() => {
                self.open_palette = None;
                self.refresh_toolbar();
            }
            None => {}
        }
    }

    pub(crate) fn close_palette(&mut self) {
        if self.open_palette.is_some() || self.palette_until.is_some() {
            self.open_palette = None;
            self.palette_until = None;
            self.palette_since = None;
            self.refresh_toolbar();
        }
    }

    pub(crate) fn set_color(&mut self, idx: usize) {
        if idx < crate::config::current().colors.len() {
            self.color_idx = idx;
            self.refresh_toolbar();
            self.recolour_selection(&crate::config::current().colors[idx]);
        }
    }

    /// Fill the closed shapes the selection holds with this palette colour, or clear them when it is
    /// `None`, and keep it in hand for the next shape. One change, the way a stroke colour pick is.
    pub(crate) fn set_fill(&mut self, fill: Option<usize>) {
        self.fill_idx = fill.filter(|index| *index < crate::config::current().colors.len());
        self.refresh_toolbar();
        let color = self
            .fill_idx
            .map(|index| crate::config::current().colors[index].clone());
        self.recolour_fill_selection(color.as_deref());
    }

    /// Open the search card, or close it when it is already open. The drawer's row and `/` both do this;
    /// the card replaces the drawer, because it is opened from one of its rows.
    pub(crate) fn toggle_search(&mut self) {
        if self.search.is_some() {
            self.search = None;
            self.refresh_toolbar();
            return;
        }
        self.close_panels();
        self.search = Some(Search::default());
        self.refresh_search();
        self.refresh_toolbar();
    }

    /// Re-run the query over the board the keyboard's output shows. The hits are indices into that
    /// board's lists, so they are recomputed whenever the query or the document changes.
    pub(crate) fn refresh_search(&mut self) {
        let Some(key) = self.keyboard_focus else {
            return;
        };
        let Some(board) = self.outputs.get(&key).map(|out| out.board().to_string()) else {
            return;
        };
        let query = self
            .search
            .as_ref()
            .map(|search| search.query.text.clone())
            .unwrap_or_default();
        let hits = self
            .doc
            .boards
            .get(&board)
            .map(|ann| ann.search(&query))
            .unwrap_or_default();
        if let Some(search) = self.search.as_mut() {
            search.hits = hits;
            search.focus = search.focus.min(search.hits.len().saturating_sub(1));
        }
    }

    /// Put the user on a search result: arm the selection tool, select the element and centre the view
    /// on it without changing the zoom — a search says where to look, not how closely. A hit whose
    /// element has gone changes nothing.
    pub(crate) fn search_pick(&mut self, index: usize) {
        let Some(hit) = self
            .search
            .as_ref()
            .and_then(|search| search.hits.get(index))
            .cloned()
        else {
            return;
        };
        let Some(key) = self.keyboard_focus else {
            return;
        };
        let Some(board) = self.outputs.get(&key).map(|out| out.board().to_string()) else {
            return;
        };
        let centre = self.doc.boards.get(&board).and_then(|ann| match hit.item {
            ItemId::Text(i) => ann.texts.get(i).map(|text| text.paint_bounds().centre()),
            ItemId::Note(i) => ann.notes.get(i).map(|note| note.bounds().centre()),
            _ => None,
        });
        let Some(centre) = centre else {
            return;
        };
        self.set_tool(Tool::Selection);
        self.set_selection(key, vec![hit.item], None);
        if let Some(out) = self.outputs.get_mut(&key) {
            let size = (out.width as f32, out.height as f32);
            out.view = out.view.centred_on(size, centre);
            out.dirty = true;
        }
        self.search = None;
        self.refresh_toolbar();
        self.dirty = true;
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
        let held = self.held_selections();
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

    /// The fill's half of `recolour_selection`: only the closed shapes the selection holds take it, and
    /// `None` takes the fill off them. Two outputs may each hold a selection, on different boards.
    fn recolour_fill_selection(&mut self, fill: Option<&str>) {
        let held = self.held_selections();
        let mut changed = Vec::new();
        let before = self.doc.clone();
        for (key, board_name, items) in held {
            if let Some(box_) = self
                .doc
                .boards
                .get_mut(&board_name)
                .and_then(|ann| ann.recolour_fill(&items, fill))
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

    /// Every output's held selection, with the board it sits on. Taken before the boards are borrowed
    /// mutably, because two outputs may hold selections on two boards.
    fn held_selections(&self) -> Vec<(u32, String, Vec<ItemId>)> {
        self.outputs
            .iter()
            .filter_map(|(key, out)| {
                out.overlay
                    .selection
                    .as_ref()
                    .map(|s| (*key, out.board().to_string(), s.items.clone()))
            })
            .collect()
    }

    /// Write the text box being edited back into the document (empty boxes are dropped).
    pub(crate) fn end_text_edit(&mut self) {
        let mut changed = false;
        let mut text_committed = false;
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
                // An empty label is not content: removing the container's text also removes the
                // binding. A label created by this edit was already inserted, so it is removed here
                // too, leaving the document exactly as the edit found it.
                if edit.item.container.is_some()
                    && let Some(index) = edit.index
                    && let Some(ann) = self.doc.boards.get_mut(&board_name)
                    && index < ann.texts.len()
                {
                    let removed = ann.texts.remove(index);
                    box_ = box_.union(view.screen_rect(removed.paint_bounds()));
                    changed = true;
                }
                out.pending = Some(match out.pending {
                    Some(pending) => pending.union(box_),
                    None => box_,
                });
                continue;
            }
            let mut item = edit.item.clone();
            item.text = edit.buffer.text.clone();
            if let Some(shape) = item.container.and_then(|id| {
                self.doc
                    .boards
                    .get(&board_name)
                    .and_then(|ann| ann.shape_index_by_id(id))
                    .and_then(|index| self.doc.boards.get(&board_name)?.shapes.get(index))
                    .cloned()
            }) {
                item = self.renderer.place_bound_text(&item, &shape);
            } else {
                item.container = None;
            }
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
            text_committed = true;
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
        if text_committed {
            self.select_after_drawing();
        }
    }

    /// Forget a pending second click when the next press cannot be one: a modifier click, or a press
    /// with another tool.
    pub(crate) fn clear_text_press(&mut self) {
        self.last_text_press = None;
        self.last_container_press = None;
    }

    /// Paint the topmost closed shape under the pointer with the fill colour in hand, or take its fill
    /// off when the hand holds none. Lines and arrows are not containers, so a press on one does nothing;
    /// a press outside every closed shape does too, and so does a press that changes nothing.
    pub(crate) fn bucket_fill(&mut self, key: u32, point: [f32; 2]) {
        let Some(board_name) = self.outputs.get(&key).map(|out| out.board().to_string()) else {
            return;
        };
        let Some((index, box_)) = self.doc.boards.get(&board_name).and_then(|ann| {
            ann.shapes
                .iter()
                .enumerate()
                .rev()
                .find(|(_, shape)| shape.contains(point))
                .map(|(index, shape)| (index, shape.paint_bounds()))
        }) else {
            return;
        };
        let fill = self
            .fill_idx
            .map(|index| crate::config::current().colors[index].clone());
        let before = self.doc.clone();
        let shape = &mut self.doc.boards.get_mut(&board_name).unwrap().shapes[index];
        if shape.fill == fill {
            return;
        }
        let painting = fill.is_some();
        shape.fill = fill;
        self.undo.record(before, &self.doc);
        self.doc_changed_at(key, box_);
        log(&format!(
            "{} shape {index}",
            if painting { "fill" } else { "clear" }
        ));
        self.dirty = true;
    }

    /// A select press on text: the first click selects and can drag the box; a quick second click on
    /// the same box opens it for writing. A press that misses text clears the pending second click.
    pub(crate) fn select_text_press(&mut self, key: u32, x: f32, y: f32) {
        let now = Instant::now();
        let Some(index) = self.text_index_at(&key, x, y) else {
            self.last_text_press = None;
            // A quick second click on the same container or line opens its bound label. This is the
            // only double-click action: point editing stays on the action card and `Enter`.
            if let Some(shape) = self.bindable_shape_at(&key, x, y) {
                if is_text_double_click(self.last_container_press, shape, [x, y], now) {
                    self.last_container_press = None;
                    self.open_bound_text(key, shape);
                } else {
                    self.last_container_press = Some(TextPress {
                        at: now,
                        point: [x, y],
                        index: shape,
                    });
                    self.select_press(key, x, y);
                }
                return;
            }
            self.last_container_press = None;
            self.select_press(key, x, y);
            return;
        };
        self.last_container_press = None;
        if is_text_double_click(self.last_text_press, index, [x, y], now) {
            self.last_text_press = None;
            self.begin_text_edit(&key, x, y);
        } else {
            self.last_text_press = Some(TextPress {
                at: now,
                point: [x, y],
                index,
            });
            self.select_press(key, x, y);
        }
    }

    /// The one linear element a selection on this output holds, if it holds exactly one.
    fn linear_selection_index(&self, key: u32) -> Option<usize> {
        let out = self.outputs.get(&key)?;
        let ann = self.doc.boards.get(out.board())?;
        selected_linear(ann, &out.overlay.selection.as_ref()?.items)
    }

    /// Turn point editing on or off for the one linear element this output holds. Choosing another
    /// tool, selection or board drops it through the same transient resets as the selection.
    pub(crate) fn set_linear_edit(&mut self, key: u32, edit: bool) {
        let Some(index) = self.linear_selection_index(key) else {
            return;
        };
        let next = edit.then_some(index);
        let Some(out) = self.outputs.get_mut(&key) else {
            return;
        };
        if out.overlay.linear_edit != next {
            out.overlay.linear_edit = next;
            out.transient_dirty = true;
            self.dirty = true;
        }
        if !edit {
            self.linear_drag = None;
            if let Some(out) = self.outputs.get_mut(&key) {
                out.overlay.linear_drag = None;
            }
        }
    }

    /// Toggle the edit mode the action card's `edit` control and `Enter` share.
    pub(crate) fn toggle_linear_edit(&mut self, key: u32) {
        self.last_container_press = None;
        let Some(index) = self.linear_selection_index(key) else {
            return;
        };
        let editing = self
            .outputs
            .get(&key)
            .is_some_and(|out| out.overlay.linear_edit == Some(index));
        self.set_linear_edit(key, !editing);
    }

    /// The topmost bindable container under a screen point: a closed shape whose interior contains
    /// it, or a line/arrow within the ordinary select tolerance. This is the target of the double
    /// click that creates or opens a bound label.
    fn bindable_shape_at(&mut self, key: &u32, x: f32, y: f32) -> Option<usize> {
        let out = self.outputs.get(key)?;
        let ann = self.doc.boards.get(out.board())?;
        let point = out.view.to_canvas([x, y]);
        if let Some(index) = ann
            .shapes
            .iter()
            .rposition(|shape| shape.kind.is_closed() && shape.contains_point(point))
        {
            return Some(index);
        }
        let limit = out.view.canvas_len(ERASER_THRESHOLD).powi(2);
        ann.shapes
            .iter()
            .enumerate()
            .rev()
            .find(|(_, shape)| {
                shape.is_linear() && shape.distance_squared(point[0], point[1]) <= limit
            })
            .map(|(index, _)| index)
    }

    /// A press on one of a linear element's point handles. A vertex drag moves that point; a
    /// midpoint press first inserts a real vertex at the midpoint, then drags it.
    fn begin_linear_handle(&mut self, key: u32, handle: LinearHandle) {
        let Some(shape_index) = self.linear_selection_index(key) else {
            return;
        };
        let Some(board_name) = self.outputs.get(&key).map(|out| out.board().to_string()) else {
            return;
        };
        let Some(shape) = self
            .doc
            .boards
            .get(&board_name)
            .and_then(|ann| ann.shapes.get(shape_index))
        else {
            return;
        };
        let before = shape.paint_bounds();
        self.undo.begin_gesture(&self.doc);
        let (vertex, end, pending, origin) = match handle {
            LinearHandle::Vertex(vertex) => {
                let end = if vertex == 0 {
                    Some(0)
                } else if vertex + 1 == shape.linear_vertex_count() {
                    Some(1)
                } else {
                    None
                };
                let origin = shape.linear_vertex(vertex).unwrap_or(shape.from);
                (vertex, end, None, origin)
            }
            LinearHandle::Midpoint(segment) => {
                let Some(point) = shape.linear_midpoints().get(segment).copied() else {
                    self.undo.abandon_gesture();
                    return;
                };
                (segment + 1, None, Some(segment), point)
            }
        };
        let mut damage = None;
        if let Some(end) = end
            && let Some(shape) = self
                .doc
                .boards
                .get_mut(&board_name)
                .and_then(|ann| ann.shapes.get_mut(shape_index))
            && shape.attach[end].take().is_some()
        {
            damage = Some(before.union(shape.paint_bounds()));
        }
        if let Some(out) = self.outputs.get_mut(&key) {
            out.transient_dirty = true;
            out.overlay.linear_drag = Some(vertex);
        }
        self.linear_drag = Some(LinearDrag {
            key,
            shape: shape_index,
            vertex,
            end,
            pending,
            origin,
        });
        if let Some(damage) = damage {
            self.doc_changed_at(key, damage);
        }
        self.dirty = true;
    }

    /// Drag the vertex taken by a point handle. The document is updated live; the press already took
    /// the undo snapshot, so the release records one change.
    pub(crate) fn drag_linear(&mut self, key: u32, x: f32, y: f32) {
        let Some(drag) = self.linear_drag else {
            return;
        };
        if drag.key != key {
            return;
        }
        let Some(out) = self.outputs.get(&key) else {
            return;
        };
        let point = out.view.to_canvas([x, y]);
        let board_name = out.board().to_string();
        let Some(shape) = self
            .doc
            .boards
            .get_mut(&board_name)
            .and_then(|ann| ann.shapes.get_mut(drag.shape))
        else {
            return;
        };
        let before = shape.paint_bounds();
        let mut drag = drag;
        if let Some(segment) = drag.pending {
            // A click is not an insertion: the pointer has to leave the midpoint handle first.
            if (point[0] - drag.origin[0]).hypot(point[1] - drag.origin[1])
                <= out.view.canvas_len(2.0)
            {
                return;
            }
            let Some(vertex) = shape.insert_linear_vertex(segment, drag.origin) else {
                return;
            };
            drag.vertex = vertex;
            drag.pending = None;
            self.linear_drag = Some(drag);
        }
        if !shape.set_linear_vertex(drag.vertex, point) {
            return;
        }
        let after = shape.paint_bounds();
        if let Some(out) = self.outputs.get_mut(&key) {
            out.overlay.linear_drag = Some(drag.vertex);
        }
        self.doc_changed_at(drag.key, before.union(after));
        self.refresh_bound_label(drag.key, drag.shape);
    }

    /// Release a point drag. An endpoint is re-seated on a closed shape when it landed within the
    /// magnetic radius, which is also how a bound end that was pulled away can bind again. The whole
    /// drag records one undo step.
    pub(crate) fn release_linear_drag(&mut self, key: u32) {
        let Some(drag) = self.linear_drag.take() else {
            return;
        };
        if drag.key != key {
            self.linear_drag = Some(drag);
            return;
        }
        if let Some(out) = self.outputs.get_mut(&key) {
            out.overlay.linear_drag = None;
            out.transient_dirty = true;
        }
        if drag.pending.is_some() {
            // The press never turned into a drag: leave the line exactly as it was.
            self.undo.abandon_gesture();
            return;
        }
        let Some(out) = self.outputs.get(&key) else {
            self.undo.abandon_gesture();
            return;
        };
        let board_name = out.board().to_string();
        let tolerance = SNAP_RADIUS / out.view.zoom;
        let Some(shape) = self
            .doc
            .boards
            .get(&board_name)
            .and_then(|ann| ann.shapes.get(drag.shape))
        else {
            self.undo.abandon_gesture();
            return;
        };
        let before = shape.paint_bounds();
        let point = shape.linear_vertex(drag.vertex).unwrap_or(shape.to);
        let snapped = drag
            .end
            .and_then(|_| self.doc.boards.get(&board_name)?.snap(point, tolerance));
        if let Some((attachment, point)) = snapped
            && let Some(shape) = self
                .doc
                .boards
                .get_mut(&board_name)
                .and_then(|ann| ann.shapes.get_mut(drag.shape))
        {
            shape.set_linear_vertex(drag.vertex, point);
            if let Some(end) = drag.end {
                shape.attach[end] = Some(attachment);
            }
        }
        let after = self
            .doc
            .boards
            .get(&board_name)
            .and_then(|ann| ann.shapes.get(drag.shape))
            .map(Shape::paint_bounds)
            .unwrap_or(before);
        self.undo.commit_gesture(&self.doc);
        self.doc_changed_at(key, before.union(after));
        self.refresh_bound_label(key, drag.shape);
    }

    /// Re-centre every label on this output's board, which is what an attached line needs after a
    /// shape move seats its endpoints. Labels are few; the scan is one per bound text.
    fn refresh_all_bound_labels(&mut self, key: u32) {
        let Some(board_name) = self.outputs.get(&key).map(|out| out.board().to_string()) else {
            return;
        };
        let indices: Vec<usize> = self
            .doc
            .boards
            .get(&board_name)
            .map(|ann| {
                ann.shapes
                    .iter()
                    .enumerate()
                    .filter(|(_, shape)| ann.bound_text(shape.id).is_some())
                    .map(|(index, _)| index)
                    .collect()
            })
            .unwrap_or_default();
        for index in indices {
            self.refresh_bound_label(key, index);
        }
    }

    /// Re-centre the label bound to one shape after its geometry moved. The text stays an ordinary
    /// text item; only its origin changes, and the caller's gesture owns the undo snapshot.
    fn refresh_bound_label(&mut self, key: u32, shape_index: usize) {
        let Some(out) = self.outputs.get(&key) else {
            return;
        };
        let board_name = out.board().to_string();
        let Some(shape) = self
            .doc
            .boards
            .get(&board_name)
            .and_then(|ann| ann.shapes.get(shape_index))
            .cloned()
        else {
            return;
        };
        let Some(text_index) = self
            .doc
            .boards
            .get(&board_name)
            .and_then(|ann| ann.bound_text_index(shape.id))
        else {
            return;
        };
        let Some(item) = self
            .doc
            .boards
            .get(&board_name)
            .and_then(|ann| ann.texts.get(text_index))
            .cloned()
        else {
            return;
        };
        let placed = self.renderer.place_bound_text(&item, &shape);
        if placed.x == item.x && placed.y == item.y {
            return;
        }
        let damage = item.paint_bounds().union(placed.paint_bounds());
        if let Some(slot) = self
            .doc
            .boards
            .get_mut(&board_name)
            .and_then(|ann| ann.texts.get_mut(text_index))
        {
            slot.x = placed.x;
            slot.y = placed.y;
        }
        self.doc_changed_at(key, damage);
    }

    /// The text box whose measured glyphs are under a canvas point. The select tool uses this to
    /// tell a single selection click from the quick second click that edits, without creating a box
    /// when the click misses.
    pub(crate) fn text_index_at(&mut self, key: &u32, x: f32, y: f32) -> Option<usize> {
        let out = self.outputs.get(key)?;
        let board_name = out.board();
        let scale = out.scale;
        let view = out.view;
        // The click is tested against the text the renderer measures, not against the font-size
        // estimate the eraser uses: that estimate is 0.6 em per character, which is too narrow
        // for CJK.
        let ann = self.doc.boards.get(board_name)?;
        ann.texts.iter().position(|t| {
            let wrap = ann.text_wrap(t);
            self.renderer
                .text_edit_bounds_wrapped(&t.text, t, wrap, Screen { scale, view })
                .contains(x, y)
        })
    }

    /// Open the label belonging to a container or line, creating an empty bound text on the first
    /// double click. An existing label is never duplicated: the reverse lookup is by the shape's
    /// stable id.
    fn open_bound_text(&mut self, key: u32, shape_index: usize) {
        let Some(board_name) = self.outputs.get(&key).map(|out| out.board().to_string()) else {
            return;
        };
        let Some(ann) = self.doc.boards.get_mut(&board_name) else {
            return;
        };
        if ann
            .shapes
            .get(shape_index)
            .is_some_and(|shape| shape.id == 0)
        {
            ann.ensure_shape_ids();
        }
        let Some(shape) = ann.shapes.get(shape_index).cloned() else {
            return;
        };
        if let Some(index) = ann.bound_text_index(shape.id) {
            let selected = self
                .outputs
                .get(&key)
                .and_then(|out| out.overlay.selection.as_ref())
                .is_some_and(|selection| selection.items.contains(&ItemId::Shape(shape_index)));
            if !selected {
                self.set_selection(key, vec![ItemId::Shape(shape_index)], None);
            }
            self.begin_text_edit_index(key, index);
            return;
        }
        // The edit's undo snapshot precedes the empty text, so a label the user abandons leaves no
        // item and no undo step; typed content is undone back to before the label existed.
        self.undo.begin_edit(&self.doc);
        let item = TextItem {
            x: shape.label_point()[0],
            y: shape.label_point()[1],
            color: shape.color.clone(),
            size: self.text_size,
            text: String::new(),
            link: None,
            container: Some(shape.id),
        };
        let item = self.renderer.place_bound_text(&item, &shape);
        let damage = item.paint_bounds().union(shape.paint_bounds());
        let index = {
            let ann = self.doc.boards.get_mut(&board_name).unwrap();
            ann.texts.push(item);
            ann.texts.len() - 1
        };
        self.set_selection(key, vec![ItemId::Shape(shape_index)], None);
        self.doc_changed_at(key, damage);
        self.begin_text_edit_index(key, index);
    }

    /// Put an already-identified text item under the caret. Used by a label, whose index exists
    /// before the edit, without creating the item again.
    fn begin_text_edit_index(&mut self, key: u32, index: usize) {
        let Some(board_name) = self.outputs.get(&key).map(|out| out.board().to_string()) else {
            return;
        };
        let Some(item) = self
            .doc
            .boards
            .get(&board_name)
            .and_then(|ann| ann.texts.get(index))
            .cloned()
        else {
            return;
        };
        self.undo.begin_edit(&self.doc);
        if let Some(out) = self.outputs.get_mut(&key) {
            out.overlay.text = Some(TextOverlay {
                buffer: TextBuffer::new(item.text.clone()),
                item,
                index: Some(index),
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

    pub(crate) fn begin_text_edit(&mut self, key: &u32, x: f32, y: f32) {
        self.last_text_press = None;
        let Some(board_name) = self.outputs.get(key).map(|out| out.board().to_string()) else {
            return;
        };
        let existing = self.text_index_at(key, x, y);
        // A note is a thing you write in, so the text tool enters it rather than dropping a text box on
        // top of it.
        let on_note = self
            .doc
            .boards
            .get(&board_name)
            .and_then(|ann| ann.notes.iter().position(|n| n.bounds().contains(x, y)));
        if let Some(index) = on_note {
            self.enter_note(key, index);
            return;
        }
        let (item, index) = match existing {
            Some(idx) => (self.doc.boards[&board_name].texts[idx].clone(), Some(idx)),
            None => (
                TextItem {
                    container: None,
                    x,
                    y,
                    color: crate::config::current().colors[self.color_idx].clone(),
                    size: self.text_size,
                    text: String::new(),
                    link: None,
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
    /// in-progress stroke is. A closed shape takes the fill colour in hand — a line or an arrow has no
    /// inside to fill — and every shape is stroked with the pen width.
    pub(crate) fn begin_shape(&mut self, key: &u32, x: f32, y: f32, kind: ShapeKind) {
        let color = crate::config::current().colors[self.color_idx].clone();
        let fill = match (kind.is_closed(), self.fill_idx) {
            (true, Some(index)) => Some(crate::config::current().colors[index].clone()),
            _ => None,
        };
        let width = self.pen_width;
        let snap = if matches!(kind, ShapeKind::Line | ShapeKind::Arrow) {
            self.snap_connector(key, [x, y])
        } else {
            None
        };
        let (from, attach) = match snap {
            Some((attachment, point)) => (point, [attachment, None]),
            None => ([x, y], [None, None]),
        };
        let Some(out) = self.outputs.get_mut(key) else {
            return;
        };
        out.overlay.shape = Some(Shape {
            roundness: (self.roundness && matches!(kind, ShapeKind::Line | ShapeKind::Arrow))
                .then_some(2),
            points: Vec::new(),
            kind,
            color,
            width,
            from,
            to: from,
            rotate: 0.0,
            id: 0,
            attach,
            fill,
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

    /// Keep the label being typed in centred on its container as the line count changes. Without
    /// this the overlay's origin stays where the empty label was placed, so the first line grows
    /// downwards out of the shape and only snaps back to the middle when the edit ends.
    pub(crate) fn recenter_bound_edit(&mut self) {
        let job = self.outputs.iter().find_map(|(key, out)| {
            let edit = out.overlay.text.as_ref()?;
            if edit.note.is_some() {
                return None;
            }
            Some((
                *key,
                out.board().to_string(),
                edit.item.clone(),
                edit.buffer.text.clone(),
            ))
        });
        let Some((key, board_name, mut item, text)) = job else {
            return;
        };
        let Some(container) = item.container else {
            return;
        };
        let Some(shape) = self.doc.boards.get(&board_name).and_then(|ann| {
            let index = ann.shape_index_by_id(container)?;
            ann.shapes.get(index).cloned()
        }) else {
            return;
        };
        item.text = text;
        let placed = self.renderer.place_bound_text(&item, &shape);
        if let Some(edit) = self
            .outputs
            .get_mut(&key)
            .and_then(|out| out.overlay.text.as_mut())
        {
            edit.item.x = placed.x;
            edit.item.y = placed.y;
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
            Command::Ai => {
                if self.open_ai(None) {
                    "ok".into()
                } else {
                    "error: no screen".into()
                }
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
        if let Some(until) = self.palette_until
            && now >= until
        {
            self.palette_until = None;
            // A pointer still resting on a colour keeps the strip open; the deadline only dismisses
            // a strip opened by a shortcut when the pointer is elsewhere. Only the stroke palette has a
            // shortcut, so only its chips can hold the strip past the deadline.
            if !matches!(self.toolbar_hover, Some(ToolbarAction::Color(_)))
                && self.open_palette.is_some()
            {
                self.open_palette = None;
                self.palette_since = None;
                self.refresh_toolbar();
            }
        }
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
        // The AI stream is drained before the store deadline: a delta that arrived while the loop was
        // idle is drawn on this tick, not on the next input event.
        self.pump_ai();
        self.animate_ai(now, dt);
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
        // Frames nobody asked for are the interesting ones: a gesture, a zoom or an answer arriving has
        // a reason to draw, a still daemon has none.
        let unasked = !self.animating() && !self.ai.streaming();
        if self.dirty {
            self.dirty = false;
            let keys: Vec<u32> = self.outputs.keys().copied().collect();
            for key in keys {
                self.draw_output(&key);
                if unasked {
                    self.frames_drawn += 1;
                }
            }
        }
        // Ten seconds of a daemon drawing ten times a second is a bug the user reads as a hot machine.
        // Nothing else notices it, so it names itself in the log, with the panel's state, and goes quiet
        // again until the next window closes just as busy.
        let window = now.saturating_duration_since(self.frame_window);
        if window >= FRAME_WINDOW {
            if self.frames_drawn as f32 / window.as_secs_f32() > BUSY_FRAMES_PER_SEC {
                log(&format!(
                    "drew {} unasked frames in {:.1} s with the AI panel {} and {} output(s)",
                    self.frames_drawn,
                    window.as_secs_f32(),
                    if self.ai.open { "open" } else { "closed" },
                    self.outputs.len()
                ));
            }
            self.frames_drawn = 0;
            self.frame_window = now;
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
                self.refresh_toolbar();
            }
        }
    }

    /// The wait until the loop is worth running again: a frame while a pop-out is opening, and
    /// the idle wait otherwise.
    fn next_wait(&self) -> Duration {
        if self.animating() {
            FRAME_WAIT
        } else if let Some(left) = self.ai.burst_hold_left(Instant::now()) {
            // The strip's number has to leave on its own clock, not on the next event.
            left.min(IDLE_WAIT)
        } else if let Some(until) = self.palette_until {
            until
                .saturating_duration_since(Instant::now())
                .min(IDLE_WAIT)
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
        // The action card exists exactly while a selection does, so its box joins the transient
        // damage and the frame that removes it paints over the card and its shadow.
        let transient = if out.overlay.selection.is_some() {
            let button = action_card_bounds((w as f32, h as f32)).grown(2.0);
            Some(transient.map_or(button, |box_| box_.union(button)))
        } else {
            transient
        };
        // The AI panel is screen-space interface: its box joins the transient damage so the frame that
        // closes it repaints every pixel it covered, and a frame that only changes the transcript
        // touches nothing else.
        let transient = match out.ai_damage {
            Some(box_) => Some(transient.map_or(box_, |other| other.union(box_))),
            None => transient,
        };
        let current_text_bounds = out.overlay.text.as_ref().and_then(|edit| {
            (edit.preedit.is_empty()).then(|| {
                let wrap = ann.text_wrap(&edit.item);
                view.screen_rect(self.renderer.text_edit_bounds_wrapped(
                    &edit.buffer.text,
                    &edit.item,
                    wrap,
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
                let surface = (w as f32, h as f32);
                box_ = box_.union(toolbar_bounds(toolbar, surface.0, surface.1));
                box_ = box_.union(cards_bounds(surface, crate::config::current().cards));
                let position = crate::config::current().toolbar;
                let along = if position.horizontal() {
                    surface.0
                } else {
                    surface.1
                };
                box_ = box_.union(color_strip_bounds(
                    position,
                    surface,
                    crate::render::rail_column(toolbar, along),
                ));
            }
            Some(box_)
        } else if let Some(text_damage) = text_damage {
            Some(text_damage)
        } else if out.transient_dirty || out.toolbar_dirty {
            // The rail's box is fixed while editing, so the previous and the current frame agree
            // on it and there is nothing to remember between frames.
            let surface = (w as f32, h as f32);
            let rail = out.toolbar_dirty.then_some(()).and(
                out.overlay
                    .toolbar
                    .as_ref()
                    .map(|toolbar| toolbar_bounds(toolbar, surface.0, surface.1)),
            );
            let mut box_ = transient_damage(out.last_transient, transient, rail);
            if out.toolbar_dirty
                && let Some(toolbar) = out.overlay.toolbar.as_ref()
            {
                box_ = box_.union(cards_bounds(surface, crate::config::current().cards));
                let position = crate::config::current().toolbar;
                let along = if position.horizontal() {
                    surface.0
                } else {
                    surface.1
                };
                box_ = box_.union(color_strip_bounds(
                    position,
                    surface,
                    crate::render::rail_column(toolbar, along),
                ));
            }
            Some(box_)
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
        out.ai_damage = None;
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
    spring_out((elapsed.as_secs_f32() / TOOLBAR_ANIM.as_secs_f32()).clamp(0.0, 1.0))
}

/// A lightly damped spring: it arrives fast, overshoots a little and settles. Used for every
/// pop-out so opening reads as a physical response, not a linear resize.
pub fn spring_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t >= 1.0 {
        return 1.0;
    }
    let (k, w) = (5.0_f32, 8.0_f32);
    1.0 - (-k * t).exp() * ((w * t).cos() + (k / w) * (w * t).sin())
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
        Tool::Pen
        | Tool::Bucket
        | Tool::Circle
        | Tool::Rect
        | Tool::Diamond
        | Tool::Line
        | Tool::Arrow => CursorShape::Crosshair,
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
        if self.ai.output == Some(key) {
            self.close_ai();
        }
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
            if self.ai.output == Some(key) {
                self.close_ai();
            }
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

    /// A second select press on the same text is an edit only when it is quick and lands close:
    /// a slow click, another box, or a drag to elsewhere stays a selection.
    #[test]
    fn only_a_quick_close_second_text_press_opens_editing() {
        let now = Instant::now();
        let first = TextPress {
            at: now,
            point: [100.0, 120.0],
            index: 2,
        };
        assert!(is_text_double_click(
            Some(first),
            2,
            [103.0, 123.0],
            now + Duration::from_millis(250)
        ));
        assert!(!is_text_double_click(
            Some(first),
            2,
            [103.0, 123.0],
            now + Duration::from_millis(500)
        ));
        assert!(!is_text_double_click(
            Some(first),
            2,
            [140.0, 123.0],
            now + Duration::from_millis(100)
        ));
        assert!(!is_text_double_click(
            Some(first),
            3,
            [103.0, 123.0],
            now + Duration::from_millis(100)
        ));
        assert!(!is_text_double_click(None, 2, [103.0, 123.0], now));
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
