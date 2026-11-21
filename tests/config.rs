//! The configuration file's settings reach the frame: the palette paints the rail's chips and the
//! note border is stroked at the thickness the file gives.
//!
//! This file has one test because the configuration is process-wide and loaded once: a second test
//! would race for the same `OnceLock` and the same environment variable.

use std::fs;

use sgraffito::canvas::{Board, Note, Overlay, Tool, Toolbar, View};
use sgraffito::render::Renderer;

#[test]
fn config_paints_the_palette_and_the_note_border() {
    let dir = std::env::temp_dir().join(format!("sgraffito-config-render-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("sgraffito")).unwrap();
    fs::write(
        dir.join("sgraffito/config.toml"),
        "border = 4\ncolors = [\"#010203\", \"#0a0b0c\", \"#0d0e0f\", \"#101112\", \"#131415\"]\n",
    )
    .unwrap();
    // SAFETY: this test binary has no other test and no other thread that reads the environment.
    unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };

    let config = sgraffito::config::init();
    assert_eq!(config.border, 4.0);
    assert_eq!(config.colors[0], "#010203", "the file was not loaded");

    // The rail's first chip is filled with the configured colour.
    let (w, h) = (500u32, 400u32);
    let mut toolbar_frame = vec![0u8; (w * h * 4) as usize];
    let toolbar = Overlay {
        toolbar: Some(Toolbar {
            tool: Tool::Pen,
            color: config.colors[0].clone(),
            size: 3.0,
            hover: None,
            grow: 0.0,
            panel_grow: 1.0,
            more: false,
            boards_open: false,
            size_open: false,
            clipboard_open: false,
            clips: Vec::new(),
            target: None,
            rename: None,
            delete_confirm: None,
            boards: Vec::new(),
            shared: false,
            backdrop: sgraffito::canvas::Backdrop::Black,
        }),
        ..Default::default()
    };
    Renderer::new().render(
        &mut toolbar_frame,
        w,
        h,
        1.0,
        View::default(),
        &Board::default(),
        &toolbar,
        None,
    );
    let painted = toolbar_frame
        .as_chunks::<4>()
        .0
        .iter()
        .any(|pixel| pixel[0] == 1 && pixel[1] == 2 && pixel[2] == 3);
    assert!(painted, "the configured chip colour is not on the rail");

    // A note's border is `border` logical pixels thick. The note is white, so its ink is the dark
    // one; a one-pixel default would leave one pixel in from the edge white.
    let (w, h) = (200u32, 200u32);
    let mut note_frame = vec![0u8; (w * h * 4) as usize];
    let board = Board {
        notes: vec![Note::new(20.0, 20.0, "#ffffff", "2026/11/18".into())],
        ..Default::default()
    };
    Renderer::new().render(
        &mut note_frame,
        w,
        h,
        1.0,
        View::default(),
        &board,
        &Overlay::default(),
        None,
    );
    let at = |x: u32, y: u32| {
        let i = ((y * w + x) * 4) as usize;
        [note_frame[i], note_frame[i + 1], note_frame[i + 2]]
    };
    assert!(
        at(21, 60).iter().all(|channel| *channel < 100),
        "the note border is not four pixels thick: {:?}",
        at(21, 60)
    );
    assert!(
        at(30, 60).iter().all(|channel| *channel > 200),
        "the note's fill is not white: {:?}",
        at(30, 60)
    );

    fs::remove_dir_all(&dir).unwrap();
}
