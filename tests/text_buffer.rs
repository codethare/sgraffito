use sgraffito::canvas::{Caret, TextBuffer};

#[test]
fn new_buffer_puts_cursor_at_end() {
    let b = TextBuffer::new("abc");
    assert_eq!(b.surrounding(), ("abc", 3));
}

#[test]
fn insert_appends_and_counts_chars_not_bytes() {
    let mut b = TextBuffer::new("");
    b.insert("买牛奶");
    assert_eq!(b.surrounding(), ("买牛奶", 3));
    b.insert("!");
    assert_eq!(b.surrounding(), ("买牛奶!", 4));
}

#[test]
fn backspace_deletes_one_multibyte_char() {
    let mut b = TextBuffer::new("买牛奶");
    assert!(b.backspace());
    assert_eq!(b.surrounding(), ("买牛", 2));
}

#[test]
fn backspace_on_empty_reports_nothing_deleted() {
    let mut b = TextBuffer::new("");
    assert!(!b.backspace());
    assert_eq!(b.surrounding(), ("", 0));
}

#[test]
fn delete_before_removes_requested_chars() {
    let mut b = TextBuffer::new("abcdef");
    assert!(b.delete_before(2));
    assert_eq!(b.surrounding(), ("abcd", 4));
    assert!(b.delete_before(99));
    assert_eq!(b.surrounding(), ("", 0));
}

#[test]
fn preedit_is_not_part_of_buffer() {
    let mut b = TextBuffer::new("hi");
    b.insert("!"); // only committed strings enter the buffer
    assert_eq!(b.surrounding(), ("hi!", 3));
    assert_eq!(b.preedit_at(), 3);
}

#[test]
fn cursor_bytes_counts_utf8_not_chars() {
    let b = TextBuffer::new("买牛奶");
    assert_eq!(b.surrounding(), ("买牛奶", 3));
    assert_eq!(b.cursor_bytes(), 9);
    assert_eq!(TextBuffer::new("hi 你好").cursor_bytes(), 9);
}

#[test]
fn byte_deletion_lands_on_a_char_boundary() {
    let mut b = TextBuffer::new("买牛奶");
    assert!(b.delete_before_bytes(6));
    assert_eq!(b.surrounding(), ("买", 1));
    // A length that would split a character deletes fewer bytes, never half a glyph.
    let mut b = TextBuffer::new("买牛奶");
    assert!(b.delete_before_bytes(7));
    assert_eq!(b.surrounding(), ("买", 1));
    // More bytes than the buffer holds clears it.
    let mut b = TextBuffer::new("买牛奶");
    assert!(b.delete_before_bytes(99));
    assert_eq!(b.surrounding(), ("", 0));
}

#[test]
fn byte_deletion_of_nothing_reports_nothing_deleted() {
    let mut b = TextBuffer::new("abc");
    assert!(!b.delete_before_bytes(0));
    assert_eq!(b.surrounding(), ("abc", 3));
    let mut empty = TextBuffer::new("");
    assert!(!empty.delete_before_bytes(4));
}

#[test]
fn insert_lands_at_the_caret() {
    let mut b = TextBuffer::new("ac");
    assert!(b.move_cursor(Caret::Left));
    b.insert("b");
    assert_eq!(b.surrounding(), ("abc", 2));
    // The caret sits after what was inserted, and the rest of the text follows it.
    b.insert("X");
    assert_eq!(b.surrounding(), ("abXc", 3));
}

#[test]
fn left_and_right_step_by_character_and_cross_lines() {
    let mut b = TextBuffer::new("买牛\ncd");
    // From the end: right does nothing, left steps back one character at a time.
    assert!(!b.move_cursor(Caret::Right));
    assert!(b.move_cursor(Caret::Left));
    assert_eq!(b.cursor, 4);
    // The caret is before "d", so the byte offset counts the three characters before it.
    assert_eq!(b.cursor_bytes(), 8);
    // Left at the start of a line lands at the end of the line before it.
    b.move_cursor(Caret::Left);
    b.move_cursor(Caret::Left);
    assert_eq!(b.surrounding(), ("买牛\ncd", 2));
    assert!(b.move_cursor(Caret::Left));
    assert_eq!(b.surrounding(), ("买牛\ncd", 1));
    // Right at the end of a line lands at the start of the next one, and left comes back.
    let mut b = TextBuffer::new("买牛\ncd");
    b.cursor = 2;
    assert!(b.move_cursor(Caret::Right));
    assert_eq!(b.surrounding(), ("买牛\ncd", 3));
    assert!(b.move_cursor(Caret::Left));
    assert_eq!(b.surrounding(), ("买牛\ncd", 2));
}

#[test]
fn up_and_down_keep_the_column_and_clamp() {
    let mut b = TextBuffer::new("abcd\ne\nfg");
    b.cursor = 3;
    assert!(b.move_cursor(Caret::Down));
    // The second line holds one character, so the column clamps to its end.
    assert_eq!(b.surrounding(), ("abcd\ne\nfg", 6));
    assert!(b.move_cursor(Caret::Down));
    assert_eq!(b.surrounding(), ("abcd\ne\nfg", 8));
    assert!(!b.move_cursor(Caret::Down));
    assert!(b.move_cursor(Caret::Up));
    assert_eq!(b.surrounding(), ("abcd\ne\nfg", 6));
}

#[test]
fn moving_the_caret_leaves_the_text_alone() {
    let mut b = TextBuffer::new("abc");
    for caret in [Caret::Left, Caret::Up, Caret::Down, Caret::Right] {
        b.move_cursor(caret);
        assert_eq!(b.text, "abc");
    }
    let mut empty = TextBuffer::new("");
    assert!(!empty.move_cursor(Caret::Left));
    assert!(!empty.move_cursor(Caret::Up));
    assert!(!empty.move_cursor(Caret::Down));
    assert!(!empty.move_cursor(Caret::Right));
    assert_eq!(empty.text, "");
}

#[test]
fn backspace_deletes_before_the_caret_only() {
    let mut b = TextBuffer::new("买牛奶!");
    b.move_cursor(Caret::Left);
    b.move_cursor(Caret::Left);
    assert!(b.backspace());
    // "牛" is gone, the caret moved back with it, and the text after the caret stays.
    assert_eq!(b.surrounding(), ("买奶!", 1));
}
