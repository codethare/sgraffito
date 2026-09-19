use sgraffito::canvas::TextBuffer;

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
