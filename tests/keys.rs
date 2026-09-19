use sgraffito::app::Tool;
use sgraffito::input::{color_for_keysym, tool_for_keysym, wants_sampling};
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
