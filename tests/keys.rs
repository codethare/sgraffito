use sgraffito::app::{MAX_PASTE_BYTES, Tool, read_selection, text_mime};
use sgraffito::canvas::Caret;
use sgraffito::input::{
    EditAction, bytes_to_delete, color_for_keysym, edit_action, size_step_for_keysym, stepped_size,
    tool_for_keysym, wants_sampling,
};
use smithay_client_toolkit::seat::keyboard::Keysym;

#[test]
fn maps_letters_to_tools_both_cases() {
    assert_eq!(tool_for_keysym(Keysym::p), Some(Tool::Pen));
    assert_eq!(tool_for_keysym(Keysym::P), Some(Tool::Pen));
    assert_eq!(tool_for_keysym(Keysym::e), Some(Tool::Eraser));
    assert_eq!(tool_for_keysym(Keysym::t), Some(Tool::Text));
    assert_eq!(tool_for_keysym(Keysym::x), None);
}

#[test]
fn maps_digits_to_palette_indices() {
    assert_eq!(color_for_keysym(Keysym::_1), Some(0));
    assert_eq!(color_for_keysym(Keysym::_5), Some(4));
    assert_eq!(color_for_keysym(Keysym::_6), None);
}

#[test]
fn drops_subpixel_jitter_but_keeps_real_motion() {
    assert!(!wants_sampling([10.0, 10.0], [10.0, 10.0]));
    assert!(!wants_sampling([10.0, 10.0], [10.5, 10.0]));
    assert!(wants_sampling([10.0, 10.0], [11.0, 10.0]));
    assert!(wants_sampling([10.0, 10.0], [10.0, 12.0]));
}

#[test]
fn maps_brackets_to_size_directions() {
    assert_eq!(size_step_for_keysym(Keysym::bracketleft), Some(-1.0));
    assert_eq!(size_step_for_keysym(Keysym::bracketright), Some(1.0));
    assert_eq!(size_step_for_keysym(Keysym::p), None);
}

#[test]
fn stepping_a_size_clamps_to_its_bounds() {
    assert_eq!(stepped_size(3.0, 1.0, 1.0, 1.0, 16.0), 4.0);
    assert_eq!(stepped_size(3.0, -1.0, 1.0, 1.0, 16.0), 2.0);
    assert_eq!(stepped_size(1.0, -1.0, 1.0, 1.0, 16.0), 1.0);
    assert_eq!(stepped_size(16.0, 1.0, 1.0, 1.0, 16.0), 16.0);
    assert_eq!(stepped_size(72.0, 1.0, 2.0, 8.0, 72.0), 72.0);
}

#[test]
fn a_focused_escape_only_ends_the_text_edit() {
    // Escape ends the text box, focused or not, preedit or not; the daemon locks on the next one.
    assert_eq!(
        edit_action(Keysym::Escape, false, false),
        EditAction::EndText
    );
    assert_eq!(
        edit_action(Keysym::Escape, false, true),
        EditAction::EndText
    );
    assert_eq!(
        edit_action(Keysym::Escape, true, false),
        EditAction::EndText
    );
}

#[test]
fn a_preedit_gives_the_input_method_every_key_but_escape() {
    for keysym in [
        Keysym::BackSpace,
        Keysym::Return,
        Keysym::p,
        Keysym::_1,
        Keysym::Left,
        Keysym::v,
    ] {
        assert_eq!(edit_action(keysym, false, true), EditAction::InputMethod);
        assert_eq!(edit_action(keysym, true, true), EditAction::InputMethod);
    }
    assert_eq!(
        edit_action(Keysym::BackSpace, false, false),
        EditAction::Backspace
    );
    assert_eq!(
        edit_action(Keysym::Return, false, false),
        EditAction::Newline
    );
    assert_eq!(edit_action(Keysym::p, false, false), EditAction::Local);
}

#[test]
fn the_arrow_keys_move_the_caret() {
    for (keysym, caret) in [
        (Keysym::Left, Caret::Left),
        (Keysym::Right, Caret::Right),
        (Keysym::Up, Caret::Up),
        (Keysym::Down, Caret::Down),
    ] {
        assert_eq!(edit_action(keysym, false, false), EditAction::Move(caret));
    }
}

#[test]
fn control_v_pastes_and_other_control_keys_do_not() {
    assert_eq!(edit_action(Keysym::v, true, false), EditAction::Paste);
    assert_eq!(edit_action(Keysym::V, true, false), EditAction::Paste);
    // Without Control, `v` is text; with it, anything else is left to the ordinary path.
    assert_eq!(edit_action(Keysym::v, false, false), EditAction::Local);
    assert_eq!(edit_action(Keysym::c, true, false), EditAction::Local);
}

#[test]
fn the_clipboard_mime_preference_is_utf8_first() {
    let offered = |mimes: &[&str]| mimes.iter().map(|m| (*m).to_string()).collect::<Vec<_>>();
    assert_eq!(
        text_mime(&offered(&["text/plain;charset=utf-8", "UTF8_STRING"])).as_deref(),
        Some("text/plain;charset=utf-8")
    );
    assert_eq!(
        text_mime(&offered(&["UTF8_STRING", "text/plain"])).as_deref(),
        Some("UTF8_STRING")
    );
    assert_eq!(
        text_mime(&offered(&["text/plain", "image/png"])).as_deref(),
        Some("text/plain")
    );
    // A selection with no plain text is not pasteable.
    assert_eq!(text_mime(&offered(&["image/png"])), None);
    assert_eq!(text_mime(&[]), None);
}

#[test]
fn a_pasted_selection_loses_its_carriage_returns() {
    assert_eq!(read_selection(&b"one\r\ntwo\rthree"[..]), "one\ntwothree");
    // Invalid UTF-8 is replaced rather than dropped, so the box keeps a character for it.
    assert_eq!(read_selection(&[0xff, b'a'][..]), "\u{fffd}a");
    // The read is capped: a selection cannot grow the box without bound.
    let huge = vec![b'x'; (MAX_PASTE_BYTES + 16) as usize];
    assert_eq!(read_selection(&huge[..]).len(), MAX_PASTE_BYTES as usize);
}

#[test]
fn delete_length_excludes_the_preedit() {
    // 6 bytes of committed text and an empty preedit: all six go.
    assert_eq!(bytes_to_delete(6, 0), 6);
    // The same request with a 3-byte preedit: only the committed part is dropped.
    assert_eq!(bytes_to_delete(6, 3), 3);
    // A request shorter than the preedit never deletes committed content.
    assert_eq!(bytes_to_delete(2, 6), 0);
}

#[test]
fn each_tool_has_its_own_pointer_shape() {
    use sgraffito::app::tool_cursor;
    use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::Shape;
    // The three shapes have to differ, or a tool change would not show on the pointer.
    assert_eq!(tool_cursor(Tool::Pen), Shape::Crosshair);
    assert_eq!(tool_cursor(Tool::Text), Shape::Text);
    assert_ne!(tool_cursor(Tool::Eraser), tool_cursor(Tool::Pen));
    assert_ne!(tool_cursor(Tool::Eraser), tool_cursor(Tool::Text));
}
