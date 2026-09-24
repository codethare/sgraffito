//! Input handling: pointer drawing, keyboard shortcuts, text input (`zwp_text_input_v3`).

use std::time::Instant;

use smithay_client_toolkit::dispatch2::Dispatch2;
use smithay_client_toolkit::seat::keyboard::{
    KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers,
};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
use wayland_client::protocol::{wl_keyboard, wl_pointer, wl_seat, wl_surface};
use wayland_client::{Connection, QueueHandle};
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::{
    self, ZwpTextInputManagerV3,
};
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::{
    self, ContentHint, ContentPurpose, ZwpTextInputV3,
};

use crate::app::{App, BTN_LEFT, Mode, PALETTE, Tool, log};
use crate::canvas::{Stroke, TextBuffer};

/// What a local key does while a text box is focused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditAction {
    /// Finish the text edit and stay in edit mode; a further `Esc` with no text box focused locks.
    EndText,
    Backspace,
    Newline,
    /// Insert the key's text into the buffer.
    Local,
    /// The input method owns this key while a preedit is active; touch nothing.
    InputMethod,
}

/// Decide a local key while a text box is focused. A non-empty preedit means the input method
/// owns every key except `Esc`, which only ends the edit, so local `Backspace`/`Enter`/printable
/// keys must not change the committed content.
pub fn edit_action(keysym: Keysym, preedit_active: bool) -> EditAction {
    if preedit_active && keysym != Keysym::Escape {
        return EditAction::InputMethod;
    }
    match keysym {
        Keysym::Escape => EditAction::EndText,
        Keysym::BackSpace => EditAction::Backspace,
        Keysym::Return | Keysym::KP_Enter => EditAction::Newline,
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
pub fn tool_for_keysym(keysym: Keysym) -> Option<Tool> {
    match keysym {
        Keysym::p | Keysym::P => Some(Tool::Pen),
        Keysym::e | Keysym::E => Some(Tool::Eraser),
        Keysym::t | Keysym::T => Some(Tool::Text),
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
            match edit_action(event.keysym, !self.preedit_is_empty()) {
                EditAction::EndText => self.end_text_edit(),
                EditAction::Backspace => self.edit_buffer(|b| {
                    b.backspace();
                }),
                EditAction::Newline => self.edit_buffer(|b| b.insert("\n")),
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
            self.set_mode(Mode::Locked);
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
        _: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
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
            match event.kind {
                PointerEventKind::Motion { .. } => self.pointer_motion(&key, x, y),
                PointerEventKind::Press { button, .. } if button == BTN_LEFT => {
                    self.pointer_press(&key, x, y)
                }
                PointerEventKind::Release { button, .. } if button == BTN_LEFT => {
                    self.pointer_release(&key)
                }
                PointerEventKind::Leave { .. } => {
                    // The release may never come back to this surface (multi-output drag), so finish here.
                    self.pointer_release(&key);
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
        self.dirty = true;
        for out in self.outputs.values_mut() {
            if out.overlay.text.is_some() {
                out.dirty = true;
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
}

impl App {
    fn pointer_press(&mut self, key: &u32, x: f32, y: f32) {
        if self.mode != Mode::Edit {
            return;
        }
        // A click anywhere finishes the text edit and any previous stroke first, so typed
        // content is kept and a new gesture starts from a clean transient state.
        self.end_text_edit();
        self.commit_transient_strokes();
        match self.tool {
            Tool::Pen => {
                if let Some(out) = self.outputs.get_mut(key) {
                    let width = self.pen_width;
                    out.overlay.stroke = Some(Stroke {
                        color: PALETTE[self.color_idx].to_string(),
                        width,
                        points: vec![[x, y]],
                    });
                }
            }
            Tool::Eraser => {
                if let Some(out) = self.outputs.get_mut(key) {
                    out.overlay.eraser = Some([x, y]);
                }
            }
            Tool::Text => self.begin_text_edit(key, x, y),
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
        let Some(out) = self.outputs.get_mut(key) else {
            return;
        };
        match self.tool {
            // Only a press starts a stroke: hovering with no button held must not draw.
            Tool::Pen => {
                let Some(stroke) = out.overlay.stroke.as_mut() else {
                    return;
                };
                if let Some(last) = stroke.points.last()
                    && !wants_sampling(*last, [x, y])
                {
                    return;
                }
                stroke.points.push([x, y]);
            }
            Tool::Eraser => out.overlay.eraser = Some([x, y]),
            Tool::Text => return,
        }
        // Only the transient overlay moved, so the next frame can damage just its box.
        out.transient_dirty = true;
        self.dirty = true;
    }

    fn pointer_release(&mut self, key: &u32) {
        let tool = self.tool;
        let id = *key;
        let bucket = self.outputs.get(key).map(|o| o.bucket(id).into_owned());
        let mut committed = false;
        if let Some(bucket) = bucket {
            let transient = self
                .outputs
                .get_mut(key)
                .map(|out| (out.overlay.stroke.take(), out.overlay.eraser.take()));
            if let Some((stroke, eraser)) = transient {
                if let Some(stroke) = stroke
                    && !stroke.points.is_empty()
                {
                    self.doc
                        .outputs
                        .entry(bucket.clone())
                        .or_default()
                        .strokes
                        .push(stroke);
                    committed = true;
                }
                if tool == Tool::Eraser
                    && let Some([x, y]) = eraser
                    && let Some(ann) = self.doc.outputs.get_mut(&bucket)
                {
                    committed |= ann.erase(x, y);
                }
            }
        }
        if committed {
            self.store.mark_dirty(Instant::now());
        }
        // The committed stroke, the erased element or the vanished marker: repaint whole.
        if let Some(out) = self.outputs.get_mut(key) {
            out.dirty = true;
        }
        self.dirty = true;
    }
}
