//! The daemon's log wiring: `init` opens the XDG state path and `write` puts a timestamped line in it.
//!
//! This is its own test binary because the logger is process-wide and initialised once; a second test
//! here would race the same `OnceLock` and environment variable.

use std::fs;

#[test]
fn init_writes_a_timestamped_line_under_the_state_home() {
    let dir = std::env::temp_dir().join(format!("sgraffito-log-init-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    // SAFETY: this binary has one test and no other thread reads the environment.
    unsafe { std::env::set_var("XDG_STATE_HOME", &dir) };

    sgraffito::log::init();
    sgraffito::log::write("a line\non one record");

    let path = dir.join("sgraffito/sgraffito.log");
    let text = fs::read_to_string(&path).expect("the log file was not written");
    assert!(
        text.contains("[sgraffito] a line\\non one record"),
        "the event is not in the file: {text:?}"
    );
    assert!(
        text.starts_with("20"),
        "the line has no date prefix: {text:?}"
    );
    assert_eq!(text.lines().count(), 1, "one event became two lines");
    let _ = fs::remove_dir_all(&dir);
}
