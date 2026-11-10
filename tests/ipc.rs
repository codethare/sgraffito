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

/// The boards are reachable from the command line because the locked surface is click-through: nothing
/// on it can be clicked, so switching boards has to be a key binding.
#[test]
fn parses_the_board_commands() {
    assert_eq!(Command::parse("boards"), Ok(Command::Boards));
    assert_eq!(
        Command::parse("board office"),
        Ok(Command::Board("office".into()))
    );
    assert_eq!(
        Command::parse("  board   news desk  "),
        Ok(Command::Board("news desk".into())),
        "a name keeps its spaces, so a board can be called what the user calls it"
    );
    assert_eq!(Command::parse("board new"), Ok(Command::NewBoard(None)));
    assert_eq!(
        Command::parse("board new office"),
        Ok(Command::NewBoard(Some("office".into())))
    );
    assert_eq!(
        Command::parse("board new two words"),
        Ok(Command::NewBoard(Some("two words".into())))
    );
    assert_eq!(Command::parse("share on"), Ok(Command::Share(true)));
    assert_eq!(Command::parse("share off"), Ok(Command::Share(false)));
}

#[test]
fn a_board_command_without_a_sensible_argument_is_refused() {
    for line in ["board", "board   ", "share", "share maybe", "share on off"] {
        assert!(Command::parse(line).is_err(), "'{line}' was accepted");
    }
    assert_eq!(
        Command::parse("board new "),
        Ok(Command::NewBoard(None)),
        "a trailing space after 'new' still names nothing, which is what an auto name is for"
    );
}

/// A board named `newton` is a board, not `new ton`: the word that creates one is a word of its own.
#[test]
fn the_word_that_creates_a_board_is_a_word_of_its_own() {
    assert_eq!(
        Command::parse("board newton"),
        Ok(Command::Board("newton".into()))
    );
}
