use sgraffito::app::Command;

#[test]
fn parses_known_commands() {
    assert_eq!(Command::parse("toggle"), Ok(Command::Toggle));
    assert_eq!(Command::parse("edit"), Ok(Command::Edit));
    assert_eq!(Command::parse("lock"), Ok(Command::Lock));
    assert_eq!(Command::parse("clear"), Ok(Command::Clear));
}

#[test]
fn tolerates_trailing_newline_and_spaces() {
    assert_eq!(Command::parse("toggle\n"), Ok(Command::Toggle));
    assert_eq!(Command::parse("  lock  "), Ok(Command::Lock));
}

#[test]
fn rejects_empty_and_unknown() {
    assert!(Command::parse("").is_err());
    assert!(Command::parse("  ").is_err());
    let err = Command::parse("frobnicate").unwrap_err();
    assert!(err.contains("frobnicate"), "{err}");
    // An error must never look like a success response
    assert!(!err.starts_with("ok"));
}

#[test]
fn is_case_sensitive() {
    assert!(Command::parse("Toggle").is_err());
}
