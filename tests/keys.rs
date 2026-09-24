use sgraffito::app::Tool;
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
    assert_eq!(edit_action(Keysym::Escape, false), EditAction::EndText);
    assert_eq!(edit_action(Keysym::Escape, true), EditAction::EndText);
}

#[test]
fn a_preedit_gives_the_input_method_every_key_but_escape() {
    for keysym in [Keysym::BackSpace, Keysym::Return, Keysym::p, Keysym::_1] {
        assert_eq!(edit_action(keysym, true), EditAction::InputMethod);
    }
    assert_eq!(edit_action(Keysym::BackSpace, false), EditAction::Backspace);
    assert_eq!(edit_action(Keysym::Return, false), EditAction::Newline);
    assert_eq!(edit_action(Keysym::p, false), EditAction::Local);
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
