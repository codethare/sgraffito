//! Input handling: pointer drawing, keyboard shortcuts, text input (`zwp_text_input_v3`).

use std::thread;
use std::time::Instant;

use smithay_client_toolkit::dispatch2::Dispatch2;
use smithay_client_toolkit::seat::keyboard::{
    KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers,
};
use smithay_client_toolkit::seat::pointer::{
    AxisScroll, PointerEvent, PointerEventKind, PointerHandler,
};
use wayland_client::protocol::{
    wl_keyboard, wl_pointer, wl_pointer::AxisSource, wl_seat, wl_surface,
};
use wayland_client::{Connection, QueueHandle};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::Shape;
use wayland_protocols::wp::pointer_gestures::zv1::client::zwp_pointer_gesture_pinch_v1::{
    self, ZwpPointerGesturePinchV1,
};
use wayland_protocols::wp::pointer_gestures::zv1::client::zwp_pointer_gestures_v1::{
    self, ZwpPointerGesturesV1,
};
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::{
    self, ZwpTextInputManagerV3,
};
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::{
    self, ContentHint, ContentPurpose, ZwpTextInputV3,
};

use crate::app::{
    App, BTN_LEFT, BTN_MIDDLE, Mode, PastedText, Tool, ToolbarAction, log, read_selection,
    text_mime,
};
use crate::canvas::{
    Board, Caret, ERASER_THRESHOLD, Rect, SELECT_HANDLE_HIT, ShapeKind, Stroke, Target, TextBuffer,
    ZOOM_STEP, constrained_drag, grab_at, selected_linear, selection_frame_with,
};
use crate::render::{
    action_card_bounds, action_card_duplicate_bounds, action_card_edit_bounds,
    action_card_move_bounds, closes_panel_on_pick, panel_stays_open, press_dismisses_panel,
};

/// The input method's own log, off unless `SGRAFFITO_IM_TRACE` is set: a commit that does not appear is a
/// question about which batch the compositor sent, and the answer has to come from the session where it
/// happens. A free function so that it can be called while a text box is borrowed for editing.
fn im_trace(message: &str) {
    if std::env::var_os("SGRAFFITO_IM_TRACE").is_some() {
        crate::app::log(&format!("im: {message}"));
    }
}

/// What a local key does while a text box is focused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditAction {
    /// Finish the text edit and stay in edit mode; a further `Esc` with no text box focused locks.
    EndText,
    Backspace,
    Newline,
    /// Move the caret inside the text box.
    Move(Caret),
    /// Paste the compositor's selection at the caret.
    Paste,
    /// Insert the key's text into the buffer.
    Local,
    /// The input method owns this key while a preedit is active; touch nothing.
    InputMethod,
}

/// Decide a local key while a text box is focused. A non-empty preedit means the input method
/// owns every key except `Esc`, which only ends the edit, so local `Backspace`/`Enter`/printable
/// keys must not change the committed content.
pub fn edit_action(keysym: Keysym, ctrl: bool, preedit_active: bool) -> EditAction {
    if preedit_active && keysym != Keysym::Escape {
        return EditAction::InputMethod;
    }
    if keysym == Keysym::Escape {
        return EditAction::EndText;
    }
    if ctrl {
        return match keysym {
            Keysym::v | Keysym::V => EditAction::Paste,
            _ => EditAction::Local,
        };
    }
    match keysym {
        Keysym::BackSpace => EditAction::Backspace,
        Keysym::Return | Keysym::KP_Enter => EditAction::Newline,
        Keysym::Left => EditAction::Move(Caret::Left),
        Keysym::Right => EditAction::Move(Caret::Right),
        Keysym::Up => EditAction::Move(Caret::Up),
        Keysym::Down => EditAction::Move(Caret::Down),
        _ => EditAction::Local,
    }
}

/// The index a row action names, when it names a row of the boards list.
fn board_index(action: ToolbarAction) -> Option<usize> {
    match action {
        ToolbarAction::Board(i)
        | ToolbarAction::AskDeleteBoard(i)
        | ToolbarAction::DeleteBoard(i)
        | ToolbarAction::RenameBoard(i) => Some(i),
        _ => None,
    }
}

/// What a key does while a board's name is being edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameKey {
    /// `Enter`: the name is taken.
    Commit,
    /// `Esc`: the name is left as it was.
    Cancel,
    /// A character, a deletion or a paste: the buffer's business.
    Buffer,
    /// Everything else: a name is one line, with no caret moves.
    Ignore,
}

/// `ok` is false while a modifier is held, and a chord is not text.
pub fn rename_key(keysym: Keysym, ok: bool) -> RenameKey {
    match keysym {
        Keysym::Return | Keysym::KP_Enter => RenameKey::Commit,
        Keysym::Escape => RenameKey::Cancel,
        // `Ctrl+V` is a paste, and a name is pasted like any other text.
        Keysym::BackSpace | Keysym::Delete => RenameKey::Buffer,
        Keysym::V => RenameKey::Buffer,
        Keysym::Left | Keysym::Right | Keysym::Up | Keysym::Down => RenameKey::Ignore,
        _ if ok => RenameKey::Buffer,
        _ => RenameKey::Ignore,
    }
}

/// `[` narrows and `]` widens the active tool's size; returns the direction as ±1.
pub fn size_step_for_keysym(keysym: Keysym) -> Option<f32> {
    match keysym {
        Keysym::bracketleft => Some(-1.0),
        Keysym::bracketright => Some(1.0),
        _ => None,
    }
}

/// Bytes `delete_surrounding_text` asks the committed buffer to drop. Its `before_length` is
/// counted from the start of the preedit when one is present (protocol note), and the preedit is
/// not in the buffer, so those bytes are not deleted.
pub fn bytes_to_delete(before_length: u32, preedit_bytes: usize) -> usize {
    (before_length as usize).saturating_sub(preedit_bytes)
}

/// Key mapping while no text box is edited: `P`/`E`/`T` switch tools.
/// What a `wl_pointer` axis event is asking the canvas for. The protocol's own source is what tells
/// the touchpad's two-finger swipe from a wheel, so no heuristic is needed and no guess is made: a
/// source we do not know changes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxisIntent {
    Pan,
    Zoom,
    Ignore,
}

pub fn axis_intent(source: Option<AxisSource>) -> AxisIntent {
    match source {
        Some(AxisSource::Finger) => AxisIntent::Pan,
        Some(AxisSource::Wheel) => AxisIntent::Zoom,
        // `Continuous` is a smooth axis that could be either device, and a tilt is another axis
        // entirely: both are left alone rather than guessed at.
        _ => AxisIntent::Ignore,
    }
}

/// One wheel event's board-list rows, with the direction the wheel reports: one row per event is
/// deliberate, so a high-resolution wheel cannot jump past the board the user is aiming at.
fn board_scroll_steps(vertical: AxisScroll) -> Option<i32> {
    if vertical.value120 != 0 {
        Some(vertical.value120.signum())
    } else if vertical.discrete != 0 {
        Some(vertical.discrete.signum())
    } else if vertical.absolute != 0.0 {
        Some(vertical.absolute.signum() as i32)
    } else {
        None
    }
}

/// The zoom factor one axis event asks for: a wheel notch is exactly one step, and a high-resolution
/// wheel is the same step to that fraction of a notch.
pub fn zoom_factor(vertical: AxisScroll, horizontal: AxisScroll) -> f32 {
    let notches = if vertical.value120 != 0 {
        vertical.value120 as f32 / 120.0
    } else if vertical.discrete != 0 {
        vertical.discrete as f32
    } else if horizontal.value120 != 0 {
        horizontal.value120 as f32 / 120.0
    } else if horizontal.discrete != 0 {
        horizontal.discrete as f32
    } else {
        // A wheel that only reports pixels: a notch is around 15 of them, which is what the
        // compositor's own default step comes to.
        0.0
    };
    ZOOM_STEP.powf(notches)
}

/// Whether this key removes the selection. `Delete` and the keypad's — as `Return` and `KP_Enter`
/// both end a line — and deliberately not `Backspace`, which deletes a character while a text box is
/// focused: one key should not mean "one character" and "everything selected" depending on the tool.
pub fn deletes_selection(keysym: Keysym) -> bool {
    matches!(keysym, Keysym::Delete | Keysym::KP_Delete)
}

/// The board list's own keys while it is open. It is a menu, so its letters act on the boards
/// rather than arming the main rail's tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoardListKey {
    Delete,
    Rename,
    New,
}

pub fn board_list_key(keysym: Keysym) -> Option<BoardListKey> {
    match keysym {
        Keysym::d | Keysym::D => Some(BoardListKey::Delete),
        Keysym::r | Keysym::R => Some(BoardListKey::Rename),
        Keysym::n | Keysym::N => Some(BoardListKey::New),
        _ => None,
    }
}

/// Key mapping while no text box is edited: `P`/`E`/`T`/`V` switch tools, and the shape tools take
/// the initials of what they draw. `V` arms select as it does in the usual drawing applications;
/// the compositor's `Ctrl+V` paste is handled before this map is reached.
pub fn tool_for_keysym(keysym: Keysym) -> Option<Tool> {
    match keysym {
        Keysym::p | Keysym::P => Some(Tool::Pen),
        Keysym::e | Keysym::E => Some(Tool::Eraser),
        Keysym::b | Keysym::B => Some(Tool::Bucket),
        Keysym::t | Keysym::T => Some(Tool::Text),
        Keysym::v | Keysym::V => Some(Tool::Selection),
        Keysym::o | Keysym::O => Some(Tool::Circle),
        Keysym::r | Keysym::R => Some(Tool::Rect),
        Keysym::d | Keysym::D => Some(Tool::Diamond),
        Keysym::l | Keysym::L => Some(Tool::Line),
        Keysym::a | Keysym::A => Some(Tool::Arrow),
        Keysym::h | Keysym::H => Some(Tool::Hand),
        Keysym::n | Keysym::N => Some(Tool::Note),
        _ => None,
    }
}

/// `M` opens and closes the drawer, `Shift+B` switches the backdrop, `C` the boards panel and `F` the
/// mode in which the screens share one board. `B` alone arms the bucket, matching Excalidraw. Mapped
/// like the other keys, so a control in a panel teaches its key exactly as a rail block does.
pub fn panel_action_for_keysym(keysym: Keysym, shift: bool) -> Option<ToolbarAction> {
    match keysym {
        Keysym::m | Keysym::M => Some(ToolbarAction::More),
        Keysym::b | Keysym::B if shift => Some(ToolbarAction::Scrim),
        Keysym::c | Keysym::C => Some(ToolbarAction::Boards),
        Keysym::f | Keysym::F => Some(ToolbarAction::Shared),
        Keysym::i | Keysym::I => Some(ToolbarAction::Ai),
        _ => None,
    }
}

/// `1`-`5` pick a colour; returns the palette index.
pub fn color_for_keysym(keysym: Keysym) -> Option<usize> {
    match keysym {
        Keysym::_1 => Some(0),
        Keysym::_2 => Some(1),
        Keysym::_3 => Some(2),
        Keysym::_4 => Some(3),
        Keysym::_5 => Some(4),
        _ => None,
    }
}

/// What a key does to the canvas when no text box is focused. With something held the arrows move it,
/// with nothing held they move the view: that is `tmmpr`'s split between moving the viewport and moving
/// the selected note, read off a state this program already has instead of a mode of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    /// Walk the selection to the next or the previous element.
    Cycle { forward: bool },
    /// Move the held selection, or the view when nothing is held: one step in that direction, `far` for
    /// the long step `Shift` gives it.
    Nudge { dx: i32, dy: i32, far: bool },
    /// Zoom the view by one step in this direction (±1).
    Zoom { dir: i32 },
    /// Put the view back at 1:1 from the origin.
    ResetView,
    /// Open the held text box or note for writing.
    Open,
}

impl Nav {
    /// Whether a key held down may repeat this: only the movement does, because a held `Esc` walking the
    /// cancel stages or a held `Tab` running through a drawing is never what a hand meant.
    pub fn repeats(self) -> bool {
        matches!(self, Nav::Nudge { .. })
    }
}

/// The keyboard's canvas keys. Compared by keysym with every other shortcut, so a shortcut is layout
/// independent; `shift` is the live modifier state, which is what tells `Shift+Tab` from `Tab`.
pub fn nav_action(keysym: Keysym, shift: bool) -> Option<Nav> {
    let nudge = |dx, dy| Some(Nav::Nudge { dx, dy, far: shift });
    match keysym {
        Keysym::Tab => Some(Nav::Cycle { forward: !shift }),
        // Some layouts send `Shift+Tab` as its own keysym rather than as `Tab` with the modifier.
        Keysym::ISO_Left_Tab => Some(Nav::Cycle { forward: false }),
        Keysym::Left => nudge(-1, 0),
        Keysym::Right => nudge(1, 0),
        Keysym::Up => nudge(0, -1),
        Keysym::Down => nudge(0, 1),
        // `=` is the unshifted key `+` sits on, and a user pressing it means the same thing.
        Keysym::plus | Keysym::equal | Keysym::KP_Add => Some(Nav::Zoom { dir: 1 }),
        Keysym::minus | Keysym::KP_Subtract => Some(Nav::Zoom { dir: -1 }),
        Keysym::_0 => Some(Nav::ResetView),
        Keysym::Return | Keysym::KP_Enter => Some(Nav::Open),
        _ => None,
    }
}

/// Sampling filter: jitter closer than 1 logical pixel to the last sample is dropped.
pub fn wants_sampling(last: [f32; 2], next: [f32; 2]) -> bool {
    let (dx, dy) = (next[0] - last[0], next[1] - last[1]);
    dx * dx + dy * dy >= 1.0
}

/// User data for `zwp_text_input_v3` and its manager.
pub struct TextInputData;

impl Dispatch2<ZwpTextInputManagerV3, App> for TextInputData {
    fn event(
        &self,
        _app: &mut App,
        _proxy: &ZwpTextInputManagerV3,
        _event: zwp_text_input_manager_v3::Event,
        _conn: &Connection,
        _qh: &QueueHandle<App>,
    ) {
    }
}

impl Dispatch2<ZwpTextInputV3, App> for TextInputData {
    fn event(
        &self,
        app: &mut App,
        _proxy: &ZwpTextInputV3,
        event: zwp_text_input_v3::Event,
        _conn: &Connection,
        _qh: &QueueHandle<App>,
    ) {
        app.text_input_event(event);
    }
}

impl KeyboardHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
        let next_focus = self.output_of(surface);
        if self.keyboard_focus.is_some() && self.keyboard_focus != next_focus {
            self.end_text_edit();
        }
        // The panel's input line belongs to the screen the keyboard is on. Moving to another screen
        // leaves the panel open and readable and takes its cursor away.
        if self.ai.open && self.ai.focus && next_focus != self.ai.output {
            self.blur_ai();
        }
        self.keyboard_focus = next_focus;
        // Focus changed, so the text input has to be enabled again.
        self.text_input_enabled = false;
        self.sync_text_input();
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if self.output_of(surface) == self.keyboard_focus {
            self.end_text_edit();
            self.keyboard_focus = None;
            // The panel keeps its keyboard until some other screen takes it: a compositor that drops
            // and re-adds focus on the same surface must not take the input line away with it.
            self.sync_text_input();
        }
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        if self.mode != Mode::Edit {
            return;
        }
        // A name being edited takes its own keys: `Enter` takes it, `Esc` leaves it, and the rest
        // belongs to the buffer.
        if self.rename.is_some() {
            match rename_key(event.keysym, !self.ctrl) {
                RenameKey::Commit => self.commit_rename(),
                RenameKey::Cancel => self.cancel_rename(),
                RenameKey::Buffer => {
                    match edit_action(event.keysym, self.ctrl, !self.preedit_is_empty()) {
                        EditAction::Backspace => self.edit_buffer(|b| {
                            b.backspace();
                        }),
                        EditAction::Paste => self.paste(),
                        EditAction::Local => {
                            if let Some(text) = event.utf8 {
                                self.edit_buffer(|b| b.insert(&text));
                            }
                        }
                        EditAction::EndText
                        | EditAction::Newline
                        | EditAction::Move(_)
                        | EditAction::InputMethod => {}
                    }
                }
                RenameKey::Ignore => {}
            }
            return;
        }
        // The AI panel's input line takes its own keys first: `Enter` sends, `Shift+Enter` adds a line,
        // `Esc` closes the panel, and everything else — including `Ctrl+V` — is the ordinary text path.
        if self.ai.open && self.ai.focus {
            if event.keysym == Keysym::Escape {
                if self.preedit_is_empty() {
                    self.close_ai();
                } else {
                    // The input method owns `Esc` while it has a preedit; all it does here is drop it.
                    self.ai.preedit.clear();
                    self.refresh_ai();
                    self.sync_text_input();
                }
                return;
            }
            if !self.preedit_is_empty() {
                // A composition in flight owns the keyboard: a key that reached the buffer now would
                // interleave with the characters the input method is about to commit.
                return;
            }
            if matches!(event.keysym, Keysym::Return | Keysym::KP_Enter) {
                if self.shift {
                    self.edit_buffer(|buffer| buffer.insert("\n"));
                } else {
                    self.send_ai();
                }
                return;
            }
        }
        // While a text box is edited, printable characters are content, not shortcuts.
        if self.edit_target_mut().is_some() {
            match edit_action(event.keysym, self.ctrl, !self.preedit_is_empty()) {
                EditAction::EndText => self.end_text_edit(),
                EditAction::Backspace => self.edit_buffer(|b| {
                    b.backspace();
                }),
                EditAction::Newline => self.edit_buffer(|b| b.insert("\n")),
                EditAction::Move(caret) => self.edit_buffer(|b| {
                    b.move_cursor(caret);
                }),
                EditAction::Paste => self.paste(),
                EditAction::Local => {
                    if let Some(text) = event.utf8 {
                        self.edit_buffer(|b| b.insert(&text));
                    }
                }
                EditAction::InputMethod => {}
            }
            return;
        }
        if self.boards_open
            && !self.ctrl
            && !self.alt
            && let Some(key) = board_list_key(event.keysym)
        {
            let acting = self
                .keyboard_focus
                .or_else(|| self.outputs.keys().next().copied());
            let Some(acting) = acting else {
                return;
            };
            let board = self.outputs.get(&acting).map(|out| out.board().to_string());
            match key {
                BoardListKey::Delete => {
                    if let Some(name) = board {
                        self.delete_board_named(&name);
                    }
                }
                BoardListKey::Rename => {
                    if let Some(name) = board {
                        let index = self.doc.names().position(|n| *n == name);
                        if let Some(index) = index {
                            self.begin_rename(index, &name);
                        }
                    }
                }
                BoardListKey::New => {
                    self.new_board(Some(acting));
                }
            }
            return;
        }
        if event.keysym == Keysym::Escape {
            self.escape();
        } else if self.ctrl && self.shift && matches!(event.keysym, Keysym::z | Keysym::Z) {
            // Redo, outside a text box, like undo below: typing keeps its own keys, and the edit
            // is the unit undo and redo work in once it has landed.
            self.redo();
        } else if self.ctrl && matches!(event.keysym, Keysym::z | Keysym::Z) {
            // Document undo, outside a text box: typing keeps its own keys, and the edit is the
            // unit undo works in once it has landed.
            self.undo();
        } else if self.ctrl && matches!(event.keysym, Keysym::d | Keysym::D) {
            // Elements are duplicated in place, not put on the compositor clipboard: `Ctrl+D`
            // clones the selection with a small offset and selects the clones.
            self.duplicate_held();
        } else if self.ctrl && matches!(event.keysym, Keysym::v | Keysym::V) {
            // `Ctrl+V` is the compositor's text selection everywhere, including a text box.
            self.paste();
        } else if deletes_selection(event.keysym) {
            // The only key that removes content. It acts on the selection, so with nothing selected
            // it does nothing.
            self.delete_selection();
        } else if let Some(action) = panel_action_for_keysym(event.keysym, self.shift) {
            // A key acts on the output the compositor gave the keyboard to, the same output the canvas
            // keys move and the rail's clicks land on. Panel keys are tested before the tool keys
            // because `Shift+B` is the backdrop while plain `B` is the bucket.
            self.apply_toolbar_action(self.keyboard_focus, action);
        } else if let Some(tool) = tool_for_keysym(event.keysym) {
            self.set_tool(tool);
        } else if let Some(idx) = color_for_keysym(event.keysym) {
            self.set_color(idx);
            self.reveal_colors();
        } else if let Some(dir) = size_step_for_keysym(event.keysym) {
            self.nudge_size(dir);
        } else if let Some(nav) = nav_action(event.keysym, self.shift) {
            self.nav_key(nav);
        }
    }

    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        if self.mode != Mode::Edit || self.edit_target_mut().is_some() {
            return;
        }
        // Only the movement keys repeat: everything else acts once per press, so a key held a fraction
        // too long cannot walk the cancel stages or the selection.
        if let Some(nav) = nav_action(event.keysym, self.shift)
            && nav.repeats()
        {
            self.nav_key(nav);
        }
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
        _: RawModifiers,
        _: u32,
    ) {
        // `Ctrl`+`V` pastes, and `Shift` and `Alt` constrain and copy a drag. The rest of the modifier
        // state is not used.
        self.ctrl = modifiers.ctrl;
        self.shift = modifiers.shift;
        self.alt = modifiers.alt;
    }
}

impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let Some(key) = self.output_of(&event.surface) else {
                continue;
            };
            let (x, y) = (event.position.0 as f32, event.position.1 as f32);
            // A pinch reports its focal point as a delta from where the fingers went down, and it is
            // dispatched outside this frame, so the last pointer position is what it measures from.
            self.pointer_at = Some((key, [x, y]));
            match event.kind {
                PointerEventKind::Motion { .. } => self.pointer_motion(&key, x, y),
                PointerEventKind::Enter { serial } => {
                    // `set_shape` is ignored unless it carries this serial, so every cursor
                    // change while the pointer is inside reuses it.
                    self.cursor_serial = Some(serial);
                    self.update_toolbar_hover(&key, x, y);
                }
                PointerEventKind::Press { button, .. } if button == BTN_LEFT => {
                    self.pointer_press(&key, x, y)
                }
                // The wheel, pressed: a pan anchored where it went down, whatever tool is armed.
                PointerEventKind::Press { button, .. } if button == BTN_MIDDLE => {
                    self.begin_pan(key, x, y)
                }
                PointerEventKind::Release { button, .. } if button == BTN_MIDDLE => self.end_pan(),
                PointerEventKind::Release { button, .. } if button == BTN_LEFT => {
                    self.pointer_release(&key)
                }
                PointerEventKind::Axis {
                    horizontal,
                    vertical,
                    source,
                    ..
                } => self.pointer_axis(key, [x, y], horizontal, vertical, source),
                PointerEventKind::Leave { .. } => {
                    // The release may never come back to this surface (multi-output drag), so finish here.
                    self.pointer_release(&key);
                    // A pan or a pinch is a view gesture with the same problem: finish it here rather
                    // than leave it holding the pointer with nothing under it.
                    self.end_pan();
                    self.pinch_end();
                    if self.toolbar_hover.take().is_some() {
                        self.refresh_toolbar_hover();
                    }
                    self.hover_colors(false);
                    self.pointer_over_toolbar = false;
                    // The tool shape must not follow the pointer onto another client's surface,
                    // and the request still needs the serial of the last enter to be honoured.
                    self.sync_cursor_default();
                    self.cursor_serial = None;
                }
                _ => {}
            }
        }
    }
}

impl App {
    /// Bind `zwp_text_input_manager_v3`; falls back to local keys when unsupported.
    pub(crate) fn bind_text_input_manager(&mut self) {
        let bound = self
            .registry_state
            .bind_one::<ZwpTextInputManagerV3, App, TextInputData>(&self.qh, 1..=1, TextInputData);
        match bound {
            Ok(manager) => self.text_input_manager = Some(manager),
            Err(e) => log(&format!(
                "compositor does not provide text-input-v3 ({e}); input methods are unavailable, falling back to local keys"
            )),
        }
    }

    /// Create `zwp_text_input_v3` once a seat shows up.
    pub(crate) fn ensure_text_input(&mut self, seat: &wl_seat::WlSeat) {
        if self.text_input.is_some() {
            return;
        }
        let Some(manager) = self.text_input_manager.clone() else {
            return;
        };
        self.text_input = Some(manager.get_text_input(seat, &self.qh, TextInputData));
    }

    /// Report the text box to the input method: enable + surrounding text when focused, disable otherwise.
    pub(crate) fn sync_text_input(&mut self) {
        let Some(ti) = self.text_input.clone() else {
            return;
        };
        let state = self
            .edit_target()
            .map(|(buffer, _)| (buffer.text.clone(), buffer.cursor_bytes()));
        match state {
            Some((text, cursor)) => {
                if !self.text_input_enabled {
                    im_trace("enable");
                    ti.enable();
                    self.text_input_enabled = true;
                }
                im_trace(&format!(
                    "surrounding {} bytes, cursor {}, buffer {:?}",
                    text.len(),
                    cursor,
                    text
                ));
                ti.set_surrounding_text(text, cursor as i32, cursor as i32);
                ti.set_content_type(ContentHint::None, ContentPurpose::Normal);
                ti.commit();
            }
            None => {
                if self.text_input_enabled {
                    im_trace("disable");
                    ti.disable();
                    ti.commit();
                    self.text_input_enabled = false;
                }
            }
        }
    }

    /// One input method batch: preedit / commit / delete followed by done; applied together on done.
    pub(crate) fn text_input_event(&mut self, event: zwp_text_input_v3::Event) {
        use zwp_text_input_v3::Event;
        match event {
            Event::Enter { .. } => {
                im_trace("enter");
                self.im_preedit = None;
                self.im_commit.clear();
                self.im_delete = None;
                self.text_input_enabled = false;
                self.sync_text_input();
            }
            Event::Leave { .. } => {
                im_trace(&format!(
                    "leave, dropping a pending preedit {:?} and commit {:?}",
                    self.im_preedit, self.im_commit
                ));
                self.im_preedit = None;
                self.im_commit.clear();
                self.im_delete = None;
                self.text_input_enabled = false;
            }
            Event::PreeditString { text, .. } => {
                im_trace(&format!("preedit {text:?}"));
                self.im_preedit = text;
            }
            Event::CommitString { text: Some(text) } => {
                im_trace(&format!("commit {text:?}"));
                self.im_commit.push_str(&text);
            }
            Event::DeleteSurroundingText {
                before_length,
                after_length,
            } => {
                im_trace(&format!(
                    "delete {before_length} before, {after_length} after"
                ));
                self.im_delete = Some((before_length, after_length));
            }
            Event::Done { serial } => {
                im_trace(&format!("done serial {serial}"));
                self.apply_text_input_batch();
            }
            _ => {}
        }
    }

    fn apply_text_input_batch(&mut self) {
        let preedit = self.im_preedit.take();
        let commit = std::mem::take(&mut self.im_commit);
        let delete = self.im_delete.take();
        let Some((buffer, preedit_slot)) = self.edit_target_mut() else {
            im_trace(&format!(
                "batch dropped, nothing is being typed into: preedit {preedit:?} commit {commit:?}"
            ));
            return;
        };
        // The preedit is double-buffered: a `done` batch that carried no `preedit_string` resets
        // it to empty, so the last pinyin does not stay rendered after its candidate was committed.
        *preedit_slot = preedit.unwrap_or_default();
        if let Some((before, _after)) = delete {
            // The cursor is always at the end, so there is never anything after it to delete.
            let bytes = bytes_to_delete(before, preedit_slot.len());
            buffer.delete_before_bytes(bytes);
        }
        if !commit.is_empty() {
            buffer.insert(&commit);
        }
        im_trace(&format!(
            "applied: preedit {:?}, commit {commit:?}, buffer {:?}",
            preedit_slot, buffer.text
        ));
        self.mark_edit_dirty();
        self.sync_text_input();
    }

    /// Mutate the text box being edited, then resync the input method and the screen.
    pub(crate) fn edit_buffer(&mut self, f: impl FnOnce(&mut TextBuffer)) {
        let Some((buffer, _)) = self.edit_target_mut() else {
            return;
        };
        f(buffer);
        self.mark_edit_dirty();
        self.sync_text_input();
    }

    /// Mark the output that owns the focused text edit for a whole-surface redraw: typed
    /// content and the caret are not localisable to a transient box.
    fn mark_edit_dirty(&mut self) {
        if self.ai.open && self.ai.focus {
            // The panel's input line is not an output's overlay text box: its own refresh is what puts
            // the typed characters, the caret and the preedit on screen.
            self.refresh_ai();
            return;
        }
        self.grow_note();
        self.recenter_bound_edit();
        self.dirty = true;
        // A name lives in the rail's snapshot, not in an output's overlay, so nothing else would put
        // the typed text on screen.
        if self.rename.is_some() {
            self.refresh_toolbar();
        }
        for out in self.outputs.values_mut() {
            if let Some(edit) = &out.overlay.text {
                if edit.preedit.is_empty() {
                    out.text_dirty = true;
                } else {
                    out.dirty = true;
                }
            }
        }
    }

    /// The buffer and preedit being typed into: a text box, a note, or a board's name. One of them at
    /// most — starting a rename ends a text edit, and a press ends a rename.
    fn edit_target_mut(&mut self) -> Option<(&mut TextBuffer, &mut String)> {
        if let Some(rename) = self.rename.as_mut() {
            return Some((&mut rename.text, &mut rename.preedit));
        }
        // The AI panel's input line is a text target too: the same caret, preedit and paste machinery
        // serve it, so the input method needs no second path.
        if self.ai.open && self.ai.focus {
            return Some((&mut self.ai.input, &mut self.ai.preedit));
        }
        let out = self
            .outputs
            .values_mut()
            .find_map(|o| o.overlay.text.as_mut())?;
        Some((&mut out.buffer, &mut out.preedit))
    }

    fn edit_target(&self) -> Option<(&TextBuffer, &str)> {
        if let Some(rename) = self.rename.as_ref() {
            return Some((&rename.text, &rename.preedit));
        }
        if self.ai.open && self.ai.focus {
            return Some((&self.ai.input, &self.ai.preedit));
        }
        let out = self
            .outputs
            .values()
            .find_map(|o| o.overlay.text.as_ref())?;
        Some((&out.buffer, &out.preedit))
    }

    fn preedit_is_empty(&self) -> bool {
        self.edit_target()
            .is_none_or(|(_, preedit)| preedit.is_empty())
    }

    /// `Esc` undoes the state the daemon is in, one step at a time. A gesture being dragged is
    /// abandoned rather than written — a stroke, a shape, a marquee or a selection drag, because a
    /// keystroke is a request to abort, unlike the interruptions that commit — then a text box under
    /// edit ends, then a selection is dropped, and only then does the daemon lock.
    fn escape(&mut self) {
        // The AI panel is the largest transient thing on screen: it goes before the rail's menus, and
        // before `Esc` locks. A focused input line has already handled its own `Esc` in `press_key`.
        if self.ai.open {
            self.close_ai();
            return;
        }
        // An open menu is the most transient thing on the screen, so it closes first: a second `Esc` then
        // does whatever it did before the menu was opened. The three panels are menus, and only one is open.
        if self.boards_open {
            self.set_boards(false);
            return;
        }
        if self.more {
            self.set_more(false);
            return;
        }
        if self.size_open {
            self.set_size(false);
            return;
        }
        let mut abandoned = self.gesture.take().is_some();
        // A drag that copied what it held made a document change, and aborting the gesture has to take it
        // back: nothing a gesture does reaches the document until it is released.
        if abandoned {
            self.drop_duplicate();
        }
        for out in self.outputs.values_mut() {
            if let Some(selection) = out.overlay.selection.as_mut() {
                // The elements are the document's again, where the document still holds them.
                selection.transform = None;
            }
            out.overlay.marquee = None;
            let dropped = out.overlay.stroke.take().is_some() | out.overlay.shape.take().is_some();
            if dropped {
                out.transient_dirty = true;
                abandoned = true;
            }
        }
        if abandoned {
            self.undo.abandon_gesture();
            self.dirty = true;
            return;
        }
        if self.edit_target().is_some() {
            self.end_text_edit();
            return;
        }
        // A line's point editing is the next transient layer: `Esc` leaves it before it drops the
        // selection the card belongs to.
        if let Some(drag) = self.linear_drag {
            self.release_linear_drag(drag.key);
        }
        let mut left_edit = false;
        for out in self.outputs.values_mut() {
            if out.overlay.linear_edit.take().is_some() {
                out.transient_dirty = true;
                left_edit = true;
            }
        }
        if left_edit {
            self.undo.abandon_gesture();
            self.dirty = true;
            return;
        }
        if self.drop_selection() {
            return;
        }
        self.set_mode(Mode::Locked);
    }

    /// The frame handle under the pointer, which is what the pointer shape says a press would
    /// grab. Only meaningful with the selection tool armed and no gesture running.
    fn update_selection_grab(&mut self, key: u32, x: f32, y: f32) {
        let grab = {
            let Some(out) = self.outputs.get(&key) else {
                return;
            };
            let board_name = out.board().to_string();
            let empty = Board::default();
            let ann = self.doc.boards.get(&board_name).unwrap_or(&empty);
            let Some(items) = out
                .overlay
                .selection
                .as_ref()
                .map(|selection| selection.items.clone())
            else {
                return;
            };
            // The handles are drawn on the measured text, so the cursor agrees with them. A line in
            // point-edit mode has only point handles; an ordinary line selection keeps the frame.
            if selected_linear(ann, &items).is_some() && out.overlay.linear_edit.is_some() {
                None
            } else {
                let boxes = self.renderer.text_boxes(ann, &items, out.screen());
                selection_frame_with(ann, &items, None, &boxes)
                    // The handles are interface lengths, so the frame is grabbed in screen space.
                    .map(|frame| out.view.screen_rect(frame))
                    .and_then(|frame| grab_at(frame, x, y, SELECT_HANDLE_HIT))
            }
        };
        if grab != self.selection_grab {
            self.selection_grab = grab;
            self.sync_cursor();
        }
    }
}

impl App {
    /// The toolbar consumes its own presses: a click on a control is not a drawing gesture, and
    /// it returns before the press reaches the tools.
    /// A scroll or a wheel. The two-finger swipe pans and the wheel zooms about the pointer; the
    /// document is not touched by either, because both are the view.
    fn pointer_axis(
        &mut self,
        key: u32,
        at: [f32; 2],
        horizontal: AxisScroll,
        vertical: AxisScroll,
        source: Option<AxisSource>,
    ) {
        // A wheel over the AI panel scrolls its transcript, and over the rest of the panel it does
        // nothing: zooming the canvas behind a panel the pointer is on is never what was meant.
        if self.ai.open
            && self.ai.output == Some(key)
            && let Some(size) = self.output_size(key)
            && let Some(toolbar) = self
                .outputs
                .get(&key)
                .map(|out| out.overlay.toolbar.clone())
        {
            let layout = crate::render::ai_panel_layout(size, toolbar.as_ref());
            if layout.panel.contains(at[0], at[1]) {
                if layout.log.contains(at[0], at[1]) {
                    let max = self.ai_max_scroll(key);
                    self.ai.scroll =
                        (self.ai.scroll + vertical.absolute as f32 * 40.0).clamp(0.0, max);
                    self.ai.follow = self.ai.scroll >= max;
                    self.refresh_ai();
                }
                return;
            }
        }
        // A wheel over the boards panel scrolls its fixed-height window. The panel has no room to
        // grow with the document, so this is how a board past the visible rows is reached. At the
        // ends the wheel is still consumed, rather than zooming the canvas behind the panel.
        if self.mode == Mode::Edit
            && self.boards_open
            && matches!(axis_intent(source), AxisIntent::Pan | AxisIntent::Zoom)
            && let Some(steps) = board_scroll_steps(vertical)
            && self.toolbar_hit(&key, at[0], at[1]).is_some_and(|action| {
                matches!(
                    action,
                    ToolbarAction::Board(_)
                        | ToolbarAction::AskDeleteBoard(_)
                        | ToolbarAction::DeleteBoard(_)
                        | ToolbarAction::RenameBoard(_)
                        | ToolbarAction::CancelDeleteBoard
                        | ToolbarAction::NewBoard
                )
            })
        {
            self.scroll_board_panel(&key, steps);
            return;
        }
        match axis_intent(source) {
            AxisIntent::Pan => {
                // 1:1 with the axis value, in the direction the compositor's own natural-scrolling
                // decision already points: the drawing follows the fingers.
                let delta = [-horizontal.absolute as f32, -vertical.absolute as f32];
                if delta != [0.0, 0.0] {
                    self.view_changed(key, |view| view.pan_by(delta));
                }
            }
            AxisIntent::Zoom => {
                let factor = zoom_factor(vertical, horizontal);
                if factor != 1.0 {
                    self.zoom_about(key, at, factor);
                }
            }
            AxisIntent::Ignore => {}
        }
    }

    fn pointer_press(&mut self, key: &u32, x: f32, y: f32) {
        if self.mode != Mode::Edit {
            return;
        }
        // The AI panel is not modal: a press inside it is the panel's, and a press outside it goes on to
        // the rail or the canvas, taking the keyboard out of the input line on the way.
        if self.ai.open
            && self.ai.output == Some(*key)
            && let Some(size) = self.output_size(*key)
            && let Some(toolbar) = self.outputs.get(key).map(|out| out.overlay.toolbar.clone())
        {
            let layout = crate::render::ai_panel_layout(size, toolbar.as_ref());
            if layout.panel.contains(x, y) {
                self.ai.pointer_captured = true;
                if layout.close.contains(x, y) {
                    self.close_ai();
                    return;
                }
                if layout.send.contains(x, y) {
                    if self.ai.streaming() {
                        self.stop_ai();
                    } else {
                        self.send_ai();
                    }
                    return;
                }
                if layout.input.contains(x, y) {
                    self.ai.focus = true;
                    self.ai.input.cursor = self.ai.input.text.chars().count();
                    self.sync_text_input();
                    self.refresh_ai();
                    return;
                }
                // The transcript and the card itself: the panel keeps the press, the input line loses
                // the keyboard.
                if self.ai.focus {
                    self.blur_ai();
                }
                return;
            }
            self.ai.pointer_captured = false;
            if self.ai.focus {
                self.blur_ai();
            }
        }
        if let Some(action) = self.toolbar_hit(key, x, y) {
            // The rows were drawn from the list as it stands, and committing a name can re-sort it, so
            // the board the press means is named before the commit moves anything.
            let board = board_index(action).and_then(|index| self.doc.names().nth(index).cloned());
            // A name being edited is committed by leaving it, the way a text box is; the lock control
            // is the rail's `Esc`, and `Esc` finishes an edit by cancelling it.
            if action != ToolbarAction::Lock {
                self.commit_rename();
            }
            // A press on another block is a press outside the open menu, which closes it; the block's own
            // action then runs, so a press on another list's block opens that one.
            if press_dismisses_panel(Some(action), self.panel_owner()) {
                self.close_panels();
            }
            self.clear_text_press();
            self.apply_toolbar_action_on(Some(*key), action, board);
            return;
        }
        // A press anywhere else is a press outside the name too, and it commits it before it applies.
        self.commit_rename();
        // A press anywhere else is a press outside the menu, and clicking outside a menu dismisses it. This
        // is also what keeps an open panel and a live text edit from existing at once: a press is what starts
        // an edit, and the panel has already gone by the time the tool below sees it. Without it, `M` with the
        // pointer over the canvas and then a click armed the text tool and entered a box with the panel still
        // open — and `Esc` then went to the edit, because a focused box handles `Esc` before the rail does, so
        // the first `Esc` ended the edit and left the menu up.
        self.close_panels();
        // `Ctrl`+click on a link is navigation, not a drawing gesture: follow it before any tool
        // sees the press. A plain click keeps doing what the armed tool does.
        if self.ctrl
            && let Some((board, centre)) = self.linked_text_at(key, x, y)
            && self.doc.boards.contains_key(&board)
        {
            self.clear_text_press();
            self.follow_link(key, &board, centre);
            return;
        }
        // The selection action card is interface: a press on it must not also start a stroke.
        if let Some(size) = self.output_size(*key)
            && self
                .outputs
                .get(key)
                .is_some_and(|out| out.overlay.selection.is_some())
            && action_card_bounds(size).contains(x, y)
        {
            if action_card_edit_bounds(size).contains(x, y) {
                self.toggle_linear_edit(*key);
            } else if action_card_duplicate_bounds(size).contains(x, y) {
                self.duplicate_on(*key);
            } else if action_card_move_bounds(size).contains(x, y) {
                self.open_target(Some(*key), Target::Move);
            }
            return;
        }
        // A pan owns the pointer while it runs: nothing else may start under it.
        if self.panning() {
            return;
        }
        // The hand tool's only gesture is a pan.
        if self.tool == Tool::Hand {
            self.begin_pan(*key, x, y);
            return;
        }
        // A click anywhere else finishes the text edit and any previous gesture first, so typed
        // content is kept and a new gesture starts from a clean transient state.
        self.end_text_edit();
        self.commit_transient_drawing();
        // The pointer speaks screen coordinates and the document speaks canvas ones, so everything
        // below is the canvas point under the pointer.
        let Some(canvas) = self.outputs.get(key).map(|out| out.view.to_canvas([x, y])) else {
            return;
        };
        let pen_snap = if self.tool == Tool::Pen {
            self.snap_connector(key, canvas)
        } else {
            None
        };
        if self.tool != Tool::Selection {
            self.clear_text_press();
        }
        match self.tool {
            Tool::Pen => {
                if let Some(out) = self.outputs.get_mut(key) {
                    let width = self.pen_width;
                    let (first, attach) = match pen_snap {
                        Some((attachment, point)) => (point, [attachment, None]),
                        None => (canvas, [None, None]),
                    };
                    out.overlay.stroke = Some(Stroke {
                        color: crate::config::current().colors[self.color_idx].clone(),
                        width,
                        points: vec![first],
                        attach,
                    });
                }
            }
            Tool::Eraser => {
                if let Some(out) = self.outputs.get_mut(key) {
                    out.overlay.eraser = Some(canvas);
                }
            }
            Tool::Bucket => self.bucket_fill(*key, canvas),
            Tool::Text => self.begin_text_edit(key, canvas[0], canvas[1]),
            Tool::Selection => {
                // One click selects and can drag a text box; a quick second click on the same glyphs
                // opens it for writing. Shift still toggles membership and Alt still copies.
                if self.shift || self.alt {
                    self.clear_text_press();
                    self.select_press(*key, x, y);
                } else {
                    self.select_text_press(*key, x, y);
                }
            }
            Tool::Circle => self.begin_shape(key, canvas[0], canvas[1], ShapeKind::Circle),
            Tool::Rect => self.begin_shape(key, canvas[0], canvas[1], ShapeKind::Rect),
            Tool::Diamond => self.begin_shape(key, canvas[0], canvas[1], ShapeKind::Diamond),
            Tool::Line => self.begin_shape(key, canvas[0], canvas[1], ShapeKind::Line),
            Tool::Arrow => self.begin_shape(key, canvas[0], canvas[1], ShapeKind::Arrow),
            Tool::Note => self.begin_note(key, canvas[0], canvas[1]),
            Tool::Hand => {}
        }
        if let Some(out) = self.outputs.get_mut(key) {
            // A press starts or ends something: repaint that output whole.
            out.dirty = true;
        }
        self.dirty = true;
    }

    fn pointer_motion(&mut self, key: &u32, x: f32, y: f32) {
        if self.ai.pointer_captured {
            return;
        }
        if self.mode != Mode::Edit {
            return;
        }
        // How fast the pointer is going decides the resolution this output's frames are drawn at, so
        // it is measured before anything reads the pointer.
        self.note_motion(key, x, y);
        // A pan takes the pointer: it does not highlight the rail and it does not draw.
        if self.panning() {
            self.drag_pan(*key, x, y);
            return;
        }
        // The toolbar is not drawing area: a drag passing over it neither draws under it nor
        // leaves the eraser marker on it, and the pointer takes the toolbar's own shape there.
        let over_toolbar = self.update_toolbar_hover(key, x, y);
        if over_toolbar {
            if let Some(out) = self.outputs.get_mut(key)
                && out.overlay.eraser.take().is_some()
            {
                out.transient_dirty = true;
                self.dirty = true;
            }
            return;
        }
        // The selection tool draws nothing on hover: the pointer only says what a press would
        // grab, until a press has started a gesture. A linear vertex is its own small drag, not a
        // selection transform, so it is checked first.
        if self.tool == Tool::Selection {
            if self.linear_drag.is_some() {
                self.drag_linear(*key, x, y);
            } else if self.gesture.is_some() {
                self.drag_selection(*key, x, y);
            } else {
                self.update_selection_grab(*key, x, y);
            }
            return;
        }
        // Read before the output is borrowed: whether `Shift` is down is what constrains the drag.
        let shift = self.shift;
        let Some(canvas) = self.outputs.get(key).map(|out| out.view.to_canvas([x, y])) else {
            return;
        };
        let snap = if matches!(self.tool, Tool::Pen | Tool::Line | Tool::Arrow) {
            self.snap_connector(key, canvas)
        } else {
            None
        };
        let Some(out) = self.outputs.get_mut(key) else {
            return;
        };
        match self.tool {
            // Only a press starts a stroke: hovering with no button held must not draw.
            Tool::Pen => {
                let Some(stroke) = out.overlay.stroke.as_mut() else {
                    return;
                };
                let (point, attach) = match snap {
                    Some((attachment, point)) => (point, attachment),
                    None => (canvas, None),
                };
                stroke.attach[1] = attach;
                if let Some(last) = stroke.points.last()
                    && !wants_sampling(*last, point)
                {
                    return;
                }
                stroke.points.push(point);
            }
            Tool::Eraser => out.overlay.eraser = Some(canvas),
            // Only a press starts a shape: motion moves the second of its two points, and `Shift`
            // constrains where that point goes. The magnetic point wins over the constrained one:
            // an end that is stuck to a shape is already where the user meant it to be.
            Tool::Circle | Tool::Rect | Tool::Diamond | Tool::Line | Tool::Arrow => {
                let Some(shape) = out.overlay.shape.as_mut() else {
                    return;
                };
                let mut to = constrained_drag(shape.kind, shape.from, canvas, shift);
                if matches!(shape.kind, ShapeKind::Line | ShapeKind::Arrow) {
                    match snap {
                        Some((attachment, point)) => {
                            shape.attach[1] = attachment;
                            to = point;
                        }
                        None => shape.attach[1] = None,
                    }
                }
                shape.to = to;
            }
            // A note is dragged out to the size it is created with, exactly as a shape is.
            Tool::Note => {
                self.drag_note(key, canvas[0], canvas[1]);
                return;
            }
            Tool::Text | Tool::Selection | Tool::Hand | Tool::Bucket => return,
        }
        // Only the transient overlay moved, so the next frame can damage just its box.
        out.transient_dirty = true;
        self.dirty = true;
    }

    /// Track the block of the toolbar under the pointer. The block stretches to say what it
    /// does, so the rail is repainted while the pointer crosses from one block to another, and
    /// the pointer takes the toolbar's own shape while it is on one.
    fn update_toolbar_hover(&mut self, key: &u32, x: f32, y: f32) -> bool {
        // The hit test uses the layout the frame drew, so a block that has already stretched
        // keeps the pointer on itself out to the end of its capsule.
        let hover = self.toolbar_hit(key, x, y);
        // The open colour card is one hover target: its plate, not just the chips, holds it open,
        // so sweeping across a gap does not fold the row before the next colour is reached.
        let over_colors =
            matches!(hover, Some(ToolbarAction::Color(_))) || self.toolbar_over_colors(key, x, y);
        // The drawer is a menu: it closes as soon as the pointer is off the `more` block and off its own
        // controls, so it never sits open beside the rail while the pointer is somewhere else. The hover is
        // worked out against the layout the frame drew, which is the open one, so the rail's own blocks are
        // still where the pointer saw them.
        if let Some(owner) = self.panel_owner()
            && !panel_stays_open(hover, Some(owner))
        {
            self.close_panels();
        }
        self.hover_colors(over_colors);
        if hover != self.toolbar_hover {
            self.toolbar_hover = hover;
            // A fresh block opens from nothing; leaving the rail closes immediately.
            self.toolbar_hover_since = hover.map(|_| Instant::now());
            self.refresh_toolbar_hover();
        }
        let over = hover.is_some() || over_colors;
        if over != self.pointer_over_toolbar {
            self.pointer_over_toolbar = over;
            self.sync_cursor();
        }
        over
    }

    /// Apply a toolbar click through the entry points the keys use, so a click and its key
    /// cannot diverge in clamping or bounds. `Esc` is the same two-stage key it is on the
    /// keyboard: the first click finishes the text box being edited, the next one locks.
    fn apply_toolbar_action(&mut self, acting: Option<u32>, action: ToolbarAction) {
        self.apply_toolbar_action_on(acting, action, None);
    }

    /// `board` is the board the press meant, named before a rename commit could re-sort the rows.
    fn apply_toolbar_action_on(
        &mut self,
        acting: Option<u32>,
        action: ToolbarAction,
        board: Option<String>,
    ) {
        if action == ToolbarAction::Lock {
            if self.rename.is_some() {
                self.cancel_rename();
            } else if self.edit_target().is_some() {
                self.end_text_edit();
            } else {
                self.set_mode(Mode::Locked);
            }
            return;
        }
        // Any other control finishes an edit in progress, exactly like a click anywhere else.
        self.end_text_edit();
        self.commit_transient_drawing();
        match action {
            ToolbarAction::Ai => {
                self.open_ai(acting);
            }
            ToolbarAction::Tool(tool) => self.set_tool(tool),
            ToolbarAction::Color(index) => {
                self.set_color(index);
                self.close_colors();
            }
            ToolbarAction::SizeStep(dir) => self.nudge_size(dir),
            ToolbarAction::More => self.toggle_more(),
            // A click on the readout toggles its list, like every other control on the rail.
            ToolbarAction::Size => self.set_size(!self.size_open),
            // No block shows these yet, but they are the same two actions the keys reach: the edit
            // in hand has already been committed above, so the snapshot they restore is the one
            // before that edit.
            ToolbarAction::Undo => {
                self.undo();
            }
            ToolbarAction::Redo => {
                self.redo();
            }
            ToolbarAction::Scrim => self.cycle_backdrop(),
            ToolbarAction::Backdrop(mode) => self.set_backdrop(mode),
            ToolbarAction::Boards => self.toggle_boards(acting),
            ToolbarAction::Shared => self.toggle_shared(),
            ToolbarAction::Link => self.open_target(acting, Target::Link),
            ToolbarAction::Round => self.toggle_roundness(acting),
            // The panel's rows are the document's boards in name order, which is the order the rail was
            // drawn from, so an index names the same board it was drawn as.
            ToolbarAction::Board(index) => {
                let picked = board.or_else(|| self.doc.names().nth(index).cloned());
                let Some(name) = picked else { return };
                self.delete_confirm = None;
                if let Some(target) = self.target.take() {
                    self.apply_target(acting, target, &name);
                } else {
                    self.switch_board(acting, &name);
                }
            }
            ToolbarAction::NewBoard => {
                self.delete_confirm = None;
                self.new_board(acting);
            }
            // The X asks first: the row itself becomes the question, and nothing is removed until the
            // `yes` button that replaces the X is pressed.
            ToolbarAction::AskDeleteBoard(index) => {
                let name = board.or_else(|| self.doc.names().nth(index).cloned());
                if let Some(name) = name {
                    self.delete_confirm = Some(name);
                    self.refresh_toolbar();
                }
            }
            ToolbarAction::DeleteBoard(index) => {
                // The row names its board by index, so a click needs no output to know what it means.
                let name = board.or_else(|| self.doc.names().nth(index).cloned());
                self.delete_confirm = None;
                if let Some(name) = name {
                    self.delete_board_named(&name);
                }
            }
            ToolbarAction::CancelDeleteBoard => {
                if self.delete_confirm.take().is_some() {
                    self.refresh_toolbar();
                }
            }
            ToolbarAction::RenameBoard(index) => {
                let name = board.or_else(|| self.doc.names().nth(index).cloned());
                if let Some(name) = name {
                    // The row's index is where that name sits now, which a re-sort may have moved.
                    let index = self.doc.names().position(|n| *n == name).unwrap_or(index);
                    self.begin_rename(index, &name);
                }
            }
            ToolbarAction::Lock => {}
        }
        // One click on something in the drawer both does the thing and puts the list away, the way a menu
        // closes after its item is chosen. A rail block needs no rule here: the pointer reached it, so the
        // drawer had already closed on the way.
        if closes_panel_on_pick(action) {
            self.close_panels();
        }
    }

    /// Paste the compositor's selection. In a text box it lands at the caret; with no editor open
    /// it becomes a new text box. There is no in-app element clipboard in the way.
    fn paste(&mut self) {
        self.refresh_system_selection();
        if !self.request_clipboard(self.edit_target().is_none()) {
            log("nothing to paste from the compositor");
        }
    }

    /// Ask the compositor for plain text. The bytes are read off the event loop: a thread reads
    /// the pipe and rings the loop when it has them, so a slow clipboard owner cannot stall
    /// drawing or the control socket. `canvas` says where a successful read belongs.
    fn request_clipboard(&mut self, canvas: bool) -> bool {
        let Some(offer) = self.selection.clone() else {
            log("nothing to paste: the compositor has no selection");
            return false;
        };
        let Some(mime) = offer.with_mime_types(text_mime) else {
            log("nothing to paste: the selection offers no plain text");
            return false;
        };
        match offer.receive(mime) {
            Ok(pipe) => {
                let sender = self.paste_tx.clone();
                let ping = self.ping.clone();
                thread::spawn(move || {
                    let text = read_selection(pipe);
                    let pasted = if canvas {
                        PastedText::Canvas(text)
                    } else {
                        PastedText::Edit(text)
                    };
                    if sender.send(pasted).is_ok() {
                        ping.ping();
                    }
                });
                true
            }
            Err(e) => {
                log(&format!("failed to read the clipboard: {e}"));
                false
            }
        }
    }

    /// Insert whatever a reader thread has finished with, through the same path a local key
    /// takes. Called from `tick`, which the ping wakes as soon as a paste is ready.
    pub(crate) fn take_pasted(&mut self) {
        while let Ok(pasted) = self.pasted.try_recv() {
            match pasted {
                PastedText::Edit(text) if self.edit_target_mut().is_some() => {
                    if !text.is_empty() {
                        self.edit_buffer(|buffer| buffer.insert(&text));
                    }
                }
                PastedText::Edit(text) | PastedText::Canvas(text) => {
                    if !text.is_empty() {
                        self.paste_text(text);
                    }
                }
            }
        }
    }

    /// Leave the pointer to the compositor after it leaves our surface.
    fn sync_cursor_default(&mut self) {
        let Some((device, serial)) = self.cursor_device.as_ref().zip(self.cursor_serial) else {
            return;
        };
        device.set_shape(serial, Shape::Default);
    }

    fn pointer_release(&mut self, key: &u32) {
        if self.ai.pointer_captured {
            self.ai.pointer_captured = false;
            return;
        }
        let tool = self.tool;
        let board_name = self.outputs.get(key).map(|o| o.board().to_string());
        // The eraser's radius is what the pointer aims with, so it is a screen length expressed in
        // the document's own units.
        let tolerance = self
            .outputs
            .get(key)
            .map(|o| o.view.canvas_len(ERASER_THRESHOLD))
            .unwrap_or(ERASER_THRESHOLD);
        // What this gesture changed, so the frame that reports it can damage just that box: a
        // committed stroke, the element the eraser removed, or both.
        let mut changed: Option<Rect> = None;
        // The snapshot precedes the stroke, shape or erase reaching the document; it is recorded
        // below only if something was actually committed, so a click that drew nothing leaves no
        // undo step.
        let before = self
            .outputs
            .get(key)
            .is_some_and(|out| {
                out.overlay
                    .stroke
                    .as_ref()
                    .is_some_and(|stroke| !stroke.points.is_empty())
                    || out
                        .overlay
                        .shape
                        .as_ref()
                        .is_some_and(|shape| !shape.is_empty())
                    || (tool == Tool::Eraser && out.overlay.eraser.is_some())
            })
            .then(|| self.doc.clone());
        if let Some(board_name) = board_name {
            let transient = self.outputs.get_mut(key).map(|out| {
                (
                    out.overlay.stroke.take(),
                    out.overlay.shape.take(),
                    out.overlay.eraser.take(),
                )
            });
            if let Some((stroke, shape, eraser)) = transient {
                if let Some(stroke) = stroke
                    && !stroke.points.is_empty()
                {
                    changed = stroke.paint_bounds();
                    let board = self.doc.boards.entry(board_name.clone()).or_default();
                    board.strokes.push(stroke);
                    if let Some(settled) = board.sync_connectors() {
                        changed = Some(changed.map_or(settled, |box_| box_.union(settled)));
                    }
                }
                // A shape dragged out to nothing is dropped rather than written, so a click with a
                // shape tool armed cannot leave an element that is invisible but still erasable.
                if let Some(shape) = shape
                    && !shape.is_empty()
                {
                    let box_ = shape.paint_bounds();
                    changed = Some(match changed {
                        Some(changed) => changed.union(box_),
                        None => box_,
                    });
                    let board = self.doc.boards.entry(board_name.clone()).or_default();
                    board.add_shape(shape);
                    if let Some(settled) = board.sync_connectors() {
                        changed = Some(changed.map_or(settled, |box_| box_.union(settled)));
                    }
                }
                if tool == Tool::Eraser
                    && let Some([x, y]) = eraser
                    && let Some(ann) = self.doc.boards.get_mut(&board_name)
                    && let Some(erased) = ann.erase(x, y, tolerance)
                {
                    changed = Some(match changed {
                        Some(changed) => changed.union(erased),
                        None => erased,
                    });
                }
            }
        }
        let drew = changed.is_some();
        match changed {
            Some(box_) => {
                if let Some(before) = before {
                    self.undo.record(before, &self.doc);
                }
                self.doc_changed_at(*key, box_)
            }
            // The eraser marker was showing and is gone, with nothing erased.
            None => self.dirty = true,
        }
        // A pen or shape tool hands the pointer to `select` after the mark that was actually drawn.
        // A click that produced nothing, and the eraser, keep their tool.
        if drew {
            self.select_after_drawing();
        }
        // The transient overlay is empty again, so this output is repainted from its own box.
        if let Some(out) = self.outputs.get_mut(key) {
            out.transient_dirty = true;
        }
        // A note being dragged out becomes content here, and its caret goes in: the note tool's promise
        // is that the next thing typed is its text.
        if self.tool == Tool::Note {
            self.commit_note(key);
        }
        // A linear point drag is its own gesture; finish it before the selection release path, which
        // only owns a transform or a marquee.
        self.release_linear_drag(*key);
        // A gesture the pointer was dragging ends here, and is written into the document.
        self.release_selection(*key);
        // A pan the left button was holding — the hand tool's — ends with the button, or the pointer
        // would go on panning the canvas as it moves with no button down at all.
        self.end_pan();
    }
}

/// User data for the pointer gesture objects. sctk wraps no part of `zwp_pointer_gestures_v1`, so the
/// manager and its pinch are dispatched here the way the text input object is.
pub struct PinchData;

impl Dispatch2<ZwpPointerGesturesV1, App> for PinchData {
    fn event(
        &self,
        _app: &mut App,
        _proxy: &ZwpPointerGesturesV1,
        _event: zwp_pointer_gestures_v1::Event,
        _conn: &Connection,
        _qh: &QueueHandle<App>,
    ) {
    }
}

impl Dispatch2<ZwpPointerGesturePinchV1, App> for PinchData {
    fn event(
        &self,
        app: &mut App,
        _proxy: &ZwpPointerGesturePinchV1,
        event: zwp_pointer_gesture_pinch_v1::Event,
        _conn: &Connection,
        _qh: &QueueHandle<App>,
    ) {
        use zwp_pointer_gesture_pinch_v1::Event;
        match event {
            Event::Begin { surface, .. } => app.pinch_begin(&surface),
            Event::Update { dx, dy, scale, .. } => {
                app.pinch_update(dx as f32, dy as f32, scale as f32)
            }
            Event::End { .. } => app.pinch_end(),
            _ => {}
        }
    }
}
