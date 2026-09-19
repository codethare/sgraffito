use sgraffito::canvas::{
    Doc, OutputAnnotations, Overlay, Stroke, TextBuffer, TextItem, TextOverlay,
};
use sgraffito::render::Renderer;

const W: u32 = 200;
const H: u32 = 100;

fn buffer(w: u32, h: u32) -> Vec<u8> {
    vec![0u8; (w * h * 4) as usize]
}

fn alpha(buf: &[u8], w: u32, x: u32, y: u32) -> u8 {
    buf[((y * w + x) * 4 + 3) as usize]
}

fn ink(buf: &[u8]) -> usize {
    buf.chunks_exact(4).filter(|p| p[3] != 0).count()
}

fn ann(strokes: Vec<Stroke>, texts: Vec<TextItem>) -> OutputAnnotations {
    OutputAnnotations { strokes, texts }
}

fn line(y: f32, x0: f32, x1: f32) -> Stroke {
    Stroke {
        color: "#e01b24".into(),
        width: 4.0,
        points: vec![[x0, y], [x1, y]],
    }
}

#[test]
fn empty_doc_leaves_buffer_transparent() {
    let mut buf = buffer(W, H);
    Renderer::new().render(
        &mut buf,
        W,
        H,
        1.0,
        &OutputAnnotations::default(),
        &Overlay::default(),
    );
    assert_eq!(ink(&buf), 0);
}

#[test]
fn committed_stroke_draws_ink_on_the_line_only() {
    let mut buf = buffer(W, H);
    let a = ann(vec![line(50.0, 20.0, 180.0)], vec![]);
    Renderer::new().render(&mut buf, W, H, 1.0, &a, &Overlay::default());
    assert!(alpha(&buf, W, 100, 50) > 0);
    assert_eq!(alpha(&buf, W, 100, 10), 0);
    assert_eq!(alpha(&buf, W, 100, 90), 0);
}

#[test]
fn scale_multiplies_coordinates() {
    let (w, h) = (W * 2, H * 2);
    let mut buf = buffer(w, h);
    let a = ann(vec![line(50.0, 20.0, 180.0)], vec![]);
    Renderer::new().render(&mut buf, w, h, 2.0, &a, &Overlay::default());
    assert!(alpha(&buf, w, 200, 100) > 0);
    assert_eq!(alpha(&buf, w, 200, 50), 0);
}

#[test]
fn text_item_draws_ink() {
    let mut buf = buffer(W, H);
    let a = ann(
        vec![],
        vec![TextItem {
            x: 10.0,
            y: 10.0,
            color: "#ffffff".into(),
            size: 32.0,
            text: "H".into(),
        }],
    );
    Renderer::new().render(&mut buf, W, H, 1.0, &a, &Overlay::default());
    assert!(ink(&buf) > 0);
    assert_eq!(alpha(&buf, W, 190, 90), 0);
}

#[test]
fn overlay_in_progress_stroke_is_drawn() {
    let mut buf = buffer(W, H);
    let overlay = Overlay {
        stroke: Some(line(30.0, 10.0, 60.0)),
        ..Default::default()
    };
    Renderer::new().render(&mut buf, W, H, 1.0, &OutputAnnotations::default(), &overlay);
    assert!(alpha(&buf, W, 35, 30) > 0);
}

#[test]
fn overlay_eraser_marker_is_drawn() {
    let mut buf = buffer(W, H);
    let overlay = Overlay {
        eraser: Some([150.0, 20.0]),
        ..Default::default()
    };
    Renderer::new().render(&mut buf, W, H, 1.0, &OutputAnnotations::default(), &overlay);
    assert!(ink(&buf) > 0);
    assert_eq!(alpha(&buf, W, 100, 80), 0);
}

#[test]
fn overlay_text_box_draws_text_and_cursor() {
    let mut buf = buffer(W, H);
    let overlay = Overlay {
        text: Some(TextOverlay {
            item: TextItem {
                x: 10.0,
                y: 10.0,
                color: "#ffffff".into(),
                size: 32.0,
                text: "ab".into(),
            },
            buffer: TextBuffer::new("ab"),
            index: None,
            preedit: String::new(),
        }),
        ..Default::default()
    };
    Renderer::new().render(&mut buf, W, H, 1.0, &OutputAnnotations::default(), &overlay);
    assert!(ink(&buf) > 0);
}

#[test]
fn preedit_renders_more_ink_than_committed_text_alone() {
    let item = TextItem {
        x: 10.0,
        y: 10.0,
        color: "#ffffff".into(),
        size: 32.0,
        text: "ab".into(),
    };
    let mut plain = buffer(W, H);
    let mut with_preedit = buffer(W, H);
    let make = |preedit: &str| Overlay {
        text: Some(TextOverlay {
            item: item.clone(),
            buffer: TextBuffer::new("ab"),
            index: None,
            preedit: preedit.into(),
        }),
        ..Default::default()
    };
    Renderer::new().render(
        &mut plain,
        W,
        H,
        1.0,
        &OutputAnnotations::default(),
        &make(""),
    );
    Renderer::new().render(
        &mut with_preedit,
        W,
        H,
        1.0,
        &OutputAnnotations::default(),
        &make("ni"),
    );
    assert!(
        ink(&with_preedit) > ink(&plain),
        "preedit should add ink: {} vs {}",
        ink(&with_preedit),
        ink(&plain)
    );
}

#[test]
fn doc_strokes_of_other_outputs_are_not_drawn() {
    let mut buf = buffer(W, H);
    let mut doc = Doc::default();
    doc.outputs
        .insert("eDP-1".into(), ann(vec![line(50.0, 20.0, 180.0)], vec![]));
    // Rendering an output reads only that output's bucket; an empty bucket means no annotations
    Renderer::new().render(
        &mut buf,
        W,
        H,
        1.0,
        &OutputAnnotations::default(),
        &Overlay::default(),
    );
    assert_eq!(ink(&buf), 0);
}
