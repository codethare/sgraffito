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
    App, BTN_LEFT, BTN_MIDDLE, Mode, PALETTE, Tool, ToolbarAction, log, read_selection, text_mime,
};
use crate::canvas::{
    Caret, ERASER_THRESHOLD, OutputAnnotations, Rect, SELECT_HANDLE_HIT, ShapeKind, Stroke,
    TextBuffer, ZOOM_STEP, grab_at, selection_frame,
};

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

/// `[` narrows and `]` widens the active tool's size; returns the direction as ±1.
pub fn size_step_for_keysym(keysym: Keysym) -> Option<f32> {
    match keysym {
        Keysym::bracketleft => Some(-1.0),
        Keysym::bracketright => Some(1.0),
        _ => None,
    }
}

/// Step a size by `dir` (±1) within `[min, max]`.
pub fn stepped_size(current: f32, dir: f32, step: f32, min: f32, max: f32) -> f32 {
    (current + dir * step).clamp(min, max)
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

/// Key mapping while no text box is edited: `P`/`E`/`T`/`S` switch tools, and the five shape tools
/// take the initials of what they draw.
pub fn tool_for_keysym(keysym: Keysym) -> Option<Tool> {
    match keysym {
        Keysym::p | Keysym::P => Some(Tool::Pen),
        Keysym::e | Keysym::E => Some(Tool::Eraser),
        Keysym::t | Keysym::T => Some(Tool::Text),
        Keysym::s | Keysym::S => Some(Tool::Selection),
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
        // While a text box is edited, printable characters are content, not shortcuts.
        if self.focused_edit_mut().is_some() {
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
        if event.keysym == Keysym::Escape {
            self.escape();
        } else if deletes_selection(event.keysym) {
            // The only key that removes content. It acts on the selection, so with nothing selected
            // it does nothing.
            self.delete_selection();
        } else if let Some(tool) = tool_for_keysym(event.keysym) {
            self.set_tool(tool);
        } else if let Some(idx) = color_for_keysym(event.keysym) {
            self.set_color(idx);
        } else if let Some(dir) = size_step_for_keysym(event.keysym) {
            self.nudge_size(dir);
        }
    }

    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
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
        // `Ctrl`+`V` pastes; the rest of the modifier state is not used yet.
        self.ctrl = modifiers.ctrl;
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
            .focused_edit()
            .map(|t| (t.buffer.text.clone(), t.buffer.cursor_bytes()));
        match state {
            Some((text, cursor)) => {
                if !self.text_input_enabled {
                    ti.enable();
                    self.text_input_enabled = true;
                }
                ti.set_surrounding_text(text, cursor as i32, cursor as i32);
                ti.set_content_type(ContentHint::None, ContentPurpose::Normal);
                ti.commit();
            }
            None => {
                if self.text_input_enabled {
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
                self.im_preedit = None;
                self.im_commit.clear();
                self.im_delete = None;
                self.text_input_enabled = false;
                self.sync_text_input();
            }
            Event::Leave { .. } => {
                self.im_preedit = None;
                self.im_commit.clear();
                self.im_delete = None;
                self.text_input_enabled = false;
            }
            Event::PreeditString { text, .. } => self.im_preedit = text,
            Event::CommitString { text: Some(text) } => self.im_commit.push_str(&text),
            Event::DeleteSurroundingText {
                before_length,
                after_length,
            } => self.im_delete = Some((before_length, after_length)),
            Event::Done { .. } => self.apply_text_input_batch(),
            _ => {}
        }
    }

    fn apply_text_input_batch(&mut self) {
        let preedit = self.im_preedit.take();
        let commit = std::mem::take(&mut self.im_commit);
        let delete = self.im_delete.take();
        let Some(edit) = self.focused_edit_mut() else {
            return;
        };
        // The preedit is double-buffered: a `done` batch that carried no `preedit_string` resets
        // it to empty, so the last pinyin does not stay rendered after its candidate was committed.
        edit.preedit = preedit.unwrap_or_default();
        if let Some((before, _after)) = delete {
            // The cursor is always at the end, so there is never anything after it to delete.
            let bytes = bytes_to_delete(before, edit.preedit.len());
            edit.buffer.delete_before_bytes(bytes);
        }
        if !commit.is_empty() {
            edit.buffer.insert(&commit);
        }
        self.mark_edit_dirty();
        self.sync_text_input();
    }

    /// Mutate the text box being edited, then resync the input method and the screen.
    pub(crate) fn edit_buffer(&mut self, f: impl FnOnce(&mut TextBuffer)) {
        let Some(edit) = self.focused_edit_mut() else {
            return;
        };
        f(&mut edit.buffer);
        self.mark_edit_dirty();
        self.sync_text_input();
    }

    /// Mark the output that owns the focused text edit for a whole-surface redraw: typed
    /// content and the caret are not localisable to a transient box.
    fn mark_edit_dirty(&mut self) {
        self.grow_note();
        self.dirty = true;
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

    fn focused_edit(&self) -> Option<&crate::canvas::TextOverlay> {
        self.outputs.values().find_map(|o| o.overlay.text.as_ref())
    }

    fn focused_edit_mut(&mut self) -> Option<&mut crate::canvas::TextOverlay> {
        self.outputs
            .values_mut()
            .find_map(|o| o.overlay.text.as_mut())
    }

    fn preedit_is_empty(&self) -> bool {
        self.focused_edit().is_none_or(|t| t.preedit.is_empty())
    }

    /// `Esc` undoes the state the daemon is in, one step at a time. A gesture being dragged is
    /// abandoned rather than written — a stroke, a shape, a marquee or a selection drag, because a
    /// keystroke is a request to abort, unlike the interruptions that commit — then a text box under
    /// edit ends, then a selection is dropped, and only then does the daemon lock.
    fn escape(&mut self) {
        let mut abandoned = self.gesture.take().is_some();
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
            self.dirty = true;
            return;
        }
        if self.focused_edit().is_some() {
            self.end_text_edit();
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
            let bucket = out.bucket(key).into_owned();
            let empty = OutputAnnotations::default();
            let ann = self.doc.outputs.get(&bucket).unwrap_or(&empty);
            out.overlay
                .selection
                .as_ref()
                .and_then(|s| selection_frame(ann, &s.items, None))
                // The handles are interface lengths, so the frame is grabbed in screen space.
                .map(|frame| out.view.screen_rect(frame))
                .and_then(|frame| grab_at(frame, x, y, SELECT_HANDLE_HIT))
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
        if let Some(action) = self.toolbar_hit(key, x, y) {
            self.apply_toolbar_action(action);
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
        match self.tool {
            Tool::Pen => {
                if let Some(out) = self.outputs.get_mut(key) {
                    let width = self.pen_width;
                    out.overlay.stroke = Some(Stroke {
                        color: PALETTE[self.color_idx].to_string(),
                        width,
                        points: vec![canvas],
                    });
                }
            }
            Tool::Eraser => {
                if let Some(out) = self.outputs.get_mut(key) {
                    out.overlay.eraser = Some(canvas);
                }
            }
            Tool::Text => self.begin_text_edit(key, canvas[0], canvas[1]),
            Tool::Selection => self.select_press(*key, x, y),
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
        if self.mode != Mode::Edit {
            return;
        }
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
        // grab, until a press has started a gesture.
        if self.tool == Tool::Selection {
            if self.gesture.is_some() {
                self.drag_selection(*key, x, y);
            } else {
                self.update_selection_grab(*key, x, y);
            }
            return;
        }
        let Some(out) = self.outputs.get_mut(key) else {
            return;
        };
        let canvas = out.view.to_canvas([x, y]);
        match self.tool {
            // Only a press starts a stroke: hovering with no button held must not draw.
            Tool::Pen => {
                let Some(stroke) = out.overlay.stroke.as_mut() else {
                    return;
                };
                if let Some(last) = stroke.points.last()
                    && !wants_sampling(*last, canvas)
                {
                    return;
                }
                stroke.points.push(canvas);
            }
            Tool::Eraser => out.overlay.eraser = Some(canvas),
            // Only a press starts a shape: motion moves the second of its two points.
            Tool::Circle | Tool::Rect | Tool::Diamond | Tool::Line | Tool::Arrow => {
                match out.overlay.shape.as_mut() {
                    Some(shape) => shape.to = canvas,
                    None => return,
                }
            }
            Tool::Text | Tool::Selection | Tool::Hand | Tool::Note => return,
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
        if hover != self.toolbar_hover {
            self.toolbar_hover = hover;
            // A fresh block opens from nothing; leaving the rail closes immediately.
            self.toolbar_hover_since = hover.map(|_| Instant::now());
            self.refresh_toolbar_hover();
        }
        let over = hover.is_some();
        if over != self.pointer_over_toolbar {
            self.pointer_over_toolbar = over;
            self.sync_cursor();
        }
        over
    }

    /// Apply a toolbar click through the entry points the keys use, so a click and its key
    /// cannot diverge in clamping or bounds. `Esc` is the same two-stage key it is on the
    /// keyboard: the first click finishes the text box being edited, the next one locks.
    fn apply_toolbar_action(&mut self, action: ToolbarAction) {
        if action == ToolbarAction::Lock {
            if self.focused_edit().is_some() {
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
            ToolbarAction::Tool(tool) => self.set_tool(tool),
            ToolbarAction::Color(index) => self.set_color(index),
            ToolbarAction::SizeStep(dir) => self.nudge_size(dir),
            ToolbarAction::Lock => {}
        }
    }

    /// Paste the compositor's selection at the caret, when a text box is being edited. The
    /// bytes are read off the event loop: a thread reads the pipe and rings the loop when it has
    /// them, so a slow clipboard owner cannot stall drawing or the control socket.
    fn paste(&mut self) {
        if self.focused_edit().is_none() {
            return;
        }
        let Some(offer) = self.selection.clone() else {
            log("nothing to paste: the compositor has no selection");
            return;
        };
        let Some(mime) = offer.with_mime_types(text_mime) else {
            log("nothing to paste: the selection offers no plain text");
            return;
        };
        match offer.receive(mime) {
            Ok(pipe) => {
                let sender = self.paste_tx.clone();
                let ping = self.ping.clone();
                thread::spawn(move || {
                    if sender.send(read_selection(pipe)).is_ok() {
                        ping.ping();
                    }
                });
            }
            Err(e) => log(&format!("failed to read the clipboard: {e}")),
        }
    }

    /// Insert whatever a reader thread has finished with, through the same path a local key
    /// takes. Called from `tick`, which the ping wakes as soon as a paste is ready.
    pub(crate) fn take_pasted(&mut self) {
        while let Ok(text) = self.pasted.try_recv() {
            if text.is_empty() {
                continue;
            }
            self.edit_buffer(|buffer| buffer.insert(&text));
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
        let tool = self.tool;
        let id = *key;
        let bucket = self.outputs.get(key).map(|o| o.bucket(id).into_owned());
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
        if let Some(bucket) = bucket {
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
                    changed = stroke.bounds();
                    self.doc
                        .outputs
                        .entry(bucket.clone())
                        .or_default()
                        .strokes
                        .push(stroke);
                }
                // A shape dragged out to nothing is dropped rather than written, so a click with a
                // shape tool armed cannot leave an element that is invisible but erasable.
                if let Some(shape) = shape
                    && !shape.is_empty()
                {
                    let box_ = shape.bounds();
                    changed = Some(match changed {
                        Some(changed) => changed.union(box_),
                        None => box_,
                    });
                    self.doc
                        .outputs
                        .entry(bucket.clone())
                        .or_default()
                        .shapes
                        .push(shape);
                }
                if tool == Tool::Eraser
                    && let Some([x, y]) = eraser
                    && let Some(ann) = self.doc.outputs.get_mut(&bucket)
                    && let Some(erased) = ann.erase(x, y, tolerance)
                {
                    changed = Some(match changed {
                        Some(changed) => changed.union(erased),
                        None => erased,
                    });
                }
            }
        }
        match changed {
            Some(box_) => self.doc_changed_at(id, box_),
            // The eraser marker was showing and is gone, with nothing erased.
            None => self.dirty = true,
        }
        // The transient overlay is empty again, so this output is repainted from its own box.
        if let Some(out) = self.outputs.get_mut(key) {
            out.transient_dirty = true;
        }
        // A gesture the pointer was dragging ends here, and is written into the document.
        self.release_selection(id);
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
