//! The AI panel's non-UI half: endpoint resolution, the Mermaid slice it understands, and the layout it
//! commits. Nothing here needs a compositor, a network or a key.

use std::collections::HashMap;
use std::time::Duration;

use sgraffito::ai::{
    AiSettings, MAX_MINDMAP_NODES, MapKind, MindNode, extract_mermaid, fit_kind, inferred_kind,
    layout_directive, layout_mindmap, parse_mindmap, resolve_with, status_message,
};
use sgraffito::canvas::ShapeKind;
use sgraffito::config::{AiConfig, Config, DEFAULT_AI_MODEL};

fn config() -> Config {
    Config::default()
}

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn resolve_env(config: &Config, pairs: &[(&str, &str)]) -> Result<AiSettings, String> {
    let map = env(pairs);
    resolve_with(config, |name| map.get(name).cloned())
}

fn tree(text: &str, children: Vec<MindNode>) -> MindNode {
    MindNode {
        text: text.to_string(),
        children,
    }
}

fn leaf(text: &str) -> MindNode {
    tree(text, Vec::new())
}

/// A stand-in for the renderer's measurement: `size` per line, about 0.6 em per character, wrapping at
/// the first width the text exceeds.
fn measure_fn() -> impl FnMut(&str, f32, Option<f32>) -> (f32, f32) {
    |text, size, wrap| measure(text, size, wrap)
}

fn measure(text: &str, size: f32, wrap: Option<f32>) -> (f32, f32) {
    let natural = text.chars().count() as f32 * size * 0.6;
    let lines = match wrap {
        Some(width) if width > 0.0 => (natural / width).ceil().max(1.0),
        _ => 1.0,
    };
    (natural, lines * size * 1.2)
}

// ---- endpoint resolution -------------------------------------------------------------------------

#[test]
fn a_go_key_picks_the_go_base_and_the_default_model() {
    let ai = resolve_env(&config(), &[("OPENCODE_GO_API_KEY", "go-key")]).unwrap();
    assert_eq!(
        ai.endpoint,
        "https://opencode.ai/inference/go/openai/v1/chat/completions"
    );
    assert_eq!(ai.model, DEFAULT_AI_MODEL);
    assert_eq!(ai.key, "go-key");
    assert_eq!(ai.timeout, Duration::from_secs(180));
}

#[test]
fn a_zen_key_picks_the_zen_base() {
    let ai = resolve_env(&config(), &[("OPENCODE_ZEN_API_KEY", "zen-key")]).unwrap();
    assert_eq!(
        ai.endpoint,
        "https://opencode.ai/inference/openai/v1/chat/completions"
    );
}

#[test]
fn the_specific_keys_win_and_an_empty_variable_is_absent() {
    let ai = resolve_env(
        &config(),
        &[
            ("OPENCODE_GO_API_KEY", "   "),
            ("OPENCODE_ZEN_API_KEY", "zen"),
            ("OPENCODE_API_KEY", "generic"),
        ],
    )
    .unwrap();
    assert_eq!(ai.key, "zen");
    assert!(
        ai.endpoint
            .starts_with("https://opencode.ai/inference/openai/")
    );

    let ai = resolve_env(&config(), &[("OPENCODE_API_KEY", "generic")]).unwrap();
    assert_eq!(ai.key, "generic");
    assert_eq!(
        ai.endpoint, "https://opencode.ai/inference/go/openai/v1/chat/completions",
        "the generic key is taken to be a Go key"
    );
}

#[test]
fn the_file_key_is_the_fallback_and_a_configured_base_overrides_the_key_base() {
    let mut config = config();
    config.ai.api_key = "file-key".into();
    let ai = resolve_env(&config, &[]).unwrap();
    assert_eq!(ai.key, "file-key");
    assert!(ai.endpoint.contains("/inference/go/"));

    config.ai.base_url = "https://gateway.example/v1".into();
    let ai = resolve_with(&config, |_| Some("env-key".into())).unwrap();
    assert_eq!(ai.key, "env-key", "the environment still wins on the key");
    assert_eq!(
        ai.endpoint, "https://gateway.example/v1/chat/completions",
        "a base that already ends in /v1 is completed, not doubled"
    );
}

/// A base that is itself an OpenAI-compatible base — a host plus `/v1` — gets only the completions
/// name. Doubling the path (`.../v1/openai/v1/chat/completions`) is what answers 404.
#[test]
fn a_versioned_base_is_completed_not_doubled() {
    let mut config = config();
    config.ai.base_url = "https://opencode.ai/zen/go/v1".into();
    let ai = resolve_env(&config, &[]).unwrap();
    assert_eq!(
        ai.endpoint,
        "https://opencode.ai/zen/go/v1/chat/completions"
    );

    config.ai.base_url = "https://opencode.ai/zen/v1/".into();
    assert_eq!(
        resolve_env(&config, &[]).unwrap().endpoint,
        "https://opencode.ai/zen/v1/chat/completions"
    );

    // A base that stops at `/openai` is the one shape that still needs the version.
    config.ai.base_url = "https://opencode.ai/inference/go/openai".into();
    assert_eq!(
        resolve_env(&config, &[]).unwrap().endpoint,
        "https://opencode.ai/inference/go/openai/v1/chat/completions"
    );

    // And the built-in Go base keeps the documented full path.
    let ai = resolve_env(&Config::default(), &[("OPENCODE_GO_API_KEY", "k")]).unwrap();
    assert_eq!(
        ai.endpoint,
        "https://opencode.ai/inference/go/openai/v1/chat/completions"
    );
}

#[test]
fn a_base_that_already_names_the_path_is_used_as_it_is() {
    let mut config = config();
    config.ai.base_url = "http://127.0.0.1:8765/chat/completions/".into();
    let ai = resolve_env(&config, &[]).unwrap();
    assert_eq!(ai.endpoint, "http://127.0.0.1:8765/chat/completions");
}

#[test]
fn nothing_to_go_on_is_an_error_that_names_the_ways_in() {
    let error = resolve_env(&config(), &[]).unwrap_err();
    for name in [
        "OPENCODE_GO_API_KEY",
        "OPENCODE_ZEN_API_KEY",
        "OPENCODE_API_KEY",
        "[ai].base_url",
    ] {
        assert!(error.contains(name), "{error}");
    }
}

#[test]
fn a_base_of_only_spaces_is_no_base_at_all() {
    let mut config = config();
    config.ai.base_url = "   ".into();
    config.ai.api_key = "  ".into();
    assert!(resolve_env(&config, &[]).is_err());
}

#[test]
fn a_configured_base_with_no_key_is_allowed() {
    let mut config = config();
    config.ai.base_url = "http://127.0.0.1:8765".into();
    let ai = resolve_env(&config, &[]).unwrap();
    assert!(ai.key.is_empty());
    assert_eq!(
        ai.endpoint,
        "http://127.0.0.1:8765/openai/v1/chat/completions"
    );
}

#[test]
fn the_file_overrides_the_model_and_the_timeout_is_clamped() {
    let mut config = config();
    config.ai.base_url = "http://127.0.0.1:1".into();
    config.ai.model = "test".into();
    config.ai.timeout = Duration::from_secs(3600);
    let ai = resolve_env(&config, &[]).unwrap();
    assert_eq!(ai.model, "test");
    assert_eq!(ai.timeout, Duration::from_secs(600));

    config.ai.timeout = Duration::from_secs(1);
    let ai = resolve_env(&config, &[]).unwrap();
    assert_eq!(ai.timeout, Duration::from_secs(5));
}

#[test]
fn an_empty_model_resolves_to_the_default() {
    let mut config = config();
    config.ai.base_url = "http://127.0.0.1:1".into();
    config.ai.model = String::new();
    let ai = resolve_env(&config, &[]).unwrap();
    assert_eq!(ai.model, DEFAULT_AI_MODEL);
}

#[test]
fn the_key_never_reaches_a_debug_line() {
    let mut config = config();
    config.ai.api_key = "sk-secret".into();
    assert!(!format!("{config:?}").contains("sk-secret"));
    assert!(format!("{config:?}").contains("<set>"));
    let ai = resolve_env(&config, &[]).unwrap();
    assert!(!format!("{ai:?}").contains("sk-secret"));
    assert_eq!(
        format!("{:?}", AiConfig::default()),
        "AiConfig { base_url: \"\", model: \"deepseek-v4.1-flash\", api_key: \"\", timeout: 180s }"
    );
}

#[test]
fn the_environment_is_read_on_every_call() {
    // The point of resolving per request: a key exported in a running session is picked up by the next
    // prompt without a restart.
    let config = config();
    let first = resolve_with(&config, |_| Some("one".into())).unwrap();
    let second = resolve_with(&config, |_| Some("two".into())).unwrap();
    assert_eq!(first.key, "one");
    assert_eq!(second.key, "two", "nothing is cached between requests");
    assert!(resolve_with(&config, |_| None).is_err());
}

// ---- Mermaid extraction ---------------------------------------------------------------------------

#[test]
fn the_last_mermaid_block_wins_over_an_earlier_attempt() {
    let answer =
        "one try:\n```mermaid\nmindmap\n  a\n```\nno, this one:\n```mermaid\nmindmap\n  b\n```\n";
    assert_eq!(
        extract_mermaid(answer).unwrap(),
        "mindmap\n  b",
        "a correction after the first block is the block to draw"
    );
}

#[test]
fn an_unterminated_block_is_taken_to_the_end() {
    let answer = "```mermaid\nmindmap\n  root((Robot))\n    Sensors\n";
    assert_eq!(
        extract_mermaid(answer).unwrap(),
        "mindmap\n  root((Robot))\n    Sensors"
    );
}

#[test]
fn a_bare_mindmap_with_no_fence_is_taken_whole() {
    assert_eq!(
        extract_mermaid("mindmap\n  root\n").unwrap(),
        "mindmap\n  root"
    );
    assert_eq!(extract_mermaid("Here is a map."), None);
    assert_eq!(extract_mermaid("```rust\nfn main() {}\n```"), None);
}

#[test]
fn other_fences_do_not_hide_the_mermaid_one() {
    let answer = "```text\nnot this\n```\n```mermaid\nmindmap\n  root\n```\n";
    assert_eq!(extract_mermaid(answer).unwrap(), "mindmap\n  root");
}

// ---- the mindmap parser ---------------------------------------------------------------------------

#[test]
fn nests_by_indentation_and_strips_the_shape_wrapper() {
    let source =
        "mindmap\n  root((Robot))\n    Sensors\n      Camera\n    Actuators\n      [Wheels]\n";
    let map = parse_mindmap(source).unwrap();
    assert_eq!(map.text, "Robot");
    assert_eq!(map.children.len(), 2);
    assert_eq!(map.children[0].text, "Sensors");
    assert_eq!(map.children[0].children[0].text, "Camera");
    assert_eq!(map.children[1].text, "Actuators");
    assert_eq!(map.children[1].children[0].text, "Wheels");
}

#[test]
fn every_wrapper_spelling_reads_as_its_text() {
    for (raw, expected) in [
        ("(a)", "a"),
        ("((b))", "b"),
        ("[c]", "c"),
        ("{d}", "d"),
        ("{{e}}", "e"),
        (")f(", "f"),
        ("))g((", "g"),
    ] {
        let source = format!("mindmap\n  root\n    {raw}\n");
        let map = parse_mindmap(&source).unwrap();
        assert_eq!(map.children[0].text, expected, "{raw}");
    }
}

#[test]
fn comments_and_icon_directives_are_ignored() {
    let source = "mindmap\n  %% a comment\n  root\n  ::icon(fa fa-book)\n    child\n";
    let map = parse_mindmap(source).unwrap();
    assert_eq!(map.text, "root");
    assert_eq!(map.children[0].text, "child");
}

#[test]
fn a_tab_counts_as_two_spaces() {
    let map = parse_mindmap("mindmap\n\troot\n\t\tchild\n").unwrap();
    assert_eq!(map.text, "root");
    assert_eq!(map.children[0].text, "child");
}

#[test]
fn a_depth_jump_is_clamped_to_one_level() {
    let map = parse_mindmap("mindmap\n  root\n        deep\n").unwrap();
    assert_eq!(map.children.len(), 1);
    assert_eq!(map.children[0].text, "deep");
}

#[test]
fn a_second_top_level_line_is_ignored() {
    let map = parse_mindmap("mindmap\n  root\n    child\nother root\n").unwrap();
    assert_eq!(map.text, "root");
    assert_eq!(map.children.len(), 1);
}

#[test]
fn a_half_received_last_line_is_dropped() {
    let map = parse_mindmap("mindmap\n  root\n    Sensors\n    Actuators((Whee").unwrap();
    assert_eq!(map.children.len(), 1, "the unfinished node is not drawn");
    assert_eq!(map.children[0].text, "Sensors");
}

#[test]
fn a_mindmap_with_no_root_is_an_error() {
    assert!(parse_mindmap("mindmap\n").is_err());
    let error = parse_mindmap("flowchart TD\n  a --> b\n").unwrap_err();
    assert!(error.contains("Mermaid mindmap"), "{error}");
}

#[test]
fn too_many_nodes_are_refused() {
    let mut source = String::from("mindmap\n  root\n");
    for index in 0..MAX_MINDMAP_NODES {
        source.push_str(&format!("    n{index}\n"));
    }
    let error = parse_mindmap(&source).unwrap_err();
    assert!(error.contains("too many"), "{error}");
    // The same map one node shorter is fine.
    let mut ok = String::from("mindmap\n  root\n");
    for index in 0..MAX_MINDMAP_NODES - 1 {
        ok.push_str(&format!("    n{index}\n"));
    }
    assert!(parse_mindmap(&ok).is_ok());
}

// ---- layout ---------------------------------------------------------------------------------------

#[test]
fn the_layout_has_one_box_and_one_line_per_node() {
    let map = tree(
        "Robot",
        vec![
            tree("Sensors", vec![leaf("Camera"), leaf("Lidar")]),
            leaf("Actuators"),
        ],
    );
    let board = layout_mindmap(
        MapKind::Radial,
        &map,
        [400.0, 300.0],
        400.0,
        "#e01b24",
        &mut measure_fn(),
    );
    let boxes = board
        .shapes
        .iter()
        .filter(|shape| shape.kind == ShapeKind::Rect)
        .count();
    let lines = board
        .shapes
        .iter()
        .filter(|shape| shape.kind == ShapeKind::Line)
        .count();
    assert_eq!(boxes, 5);
    assert_eq!(lines, 4);
    assert_eq!(board.texts.len(), 5);
    assert!(board.strokes.is_empty(), "nothing but the diagram");
    assert!(board.notes.is_empty());
}

#[test]
fn every_label_names_a_box_and_every_line_attaches_to_two_of_them() {
    let map = tree("root", vec![leaf("a"), tree("b", vec![leaf("c")])]);
    let board = layout_mindmap(
        MapKind::Radial,
        &map,
        [0.0, 0.0],
        400.0,
        "#3584e4",
        &mut measure_fn(),
    );
    let boxes: Vec<u64> = board
        .shapes
        .iter()
        .filter(|shape| shape.kind == ShapeKind::Rect)
        .map(|shape| shape.id)
        .collect();
    for text in &board.texts {
        let container = text.container.expect("a node label is bound to its box");
        assert!(boxes.contains(&container), "label names {container}");
    }
    for line in board
        .shapes
        .iter()
        .filter(|shape| shape.kind == ShapeKind::Line)
    {
        let ends = line.attach;
        for attachment in ends.into_iter().flatten() {
            assert!(
                boxes.contains(&attachment.shape),
                "line attaches to {attachment:?}"
            );
        }
        assert!(ends[0].is_some() && ends[1].is_some());
    }
}

#[test]
fn the_ids_are_unique_and_every_coordinate_is_finite() {
    let map = tree("root", (0..8).map(|i| leaf(&format!("n{i}"))).collect());
    let board = layout_mindmap(
        MapKind::Radial,
        &map,
        [100.0, 50.0],
        400.0,
        "#f6d32d",
        &mut measure_fn(),
    );
    let mut ids: Vec<u64> = board.shapes.iter().map(|shape| shape.id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), board.shapes.len());
    for shape in &board.shapes {
        for point in [shape.from, shape.to] {
            assert!(point[0].is_finite() && point[1].is_finite(), "{point:?}");
        }
    }
    for text in &board.texts {
        assert!(text.x.is_finite() && text.y.is_finite());
    }
}

#[test]
fn the_root_sits_at_the_anchor_and_children_ring_around_it() {
    let map = tree("root", vec![leaf("a"), leaf("b")]);
    let board = layout_mindmap(
        MapKind::Radial,
        &map,
        [50.0, 175.0],
        400.0,
        "#33d17a",
        &mut measure_fn(),
    );
    let root = board
        .shapes
        .iter()
        .find(|shape| shape.kind == ShapeKind::Rect && shape.id == 1)
        .unwrap();
    assert_eq!(root.centre(), [250.0, 175.0]);
    for text in board.texts.iter().skip(1) {
        let centre = [
            text.x + measure(&text.text, 16.0, None).0 / 2.0,
            text.y + measure(&text.text, 16.0, None).1 / 2.0,
        ];
        let radius = ((centre[0] - 250.0).powi(2) + (centre[1] - 175.0).powi(2)).sqrt();
        assert!((radius - 190.0).abs() < 8.0, "child at {centre:?}");
    }
}

#[test]
fn one_node_still_lays_out_as_a_box_with_its_label() {
    let board = layout_mindmap(
        MapKind::Radial,
        &leaf("alone"),
        [0.0, 0.0],
        400.0,
        "#ffffff",
        &mut measure_fn(),
    );
    assert_eq!(board.shapes.len(), 1);
    assert_eq!(board.texts.len(), 1);
    assert_eq!(board.texts[0].container, Some(1));
}

// ---- what a failed request says ----------------------------------------------------------------

#[test]
fn a_401_with_no_key_names_the_ways_to_supply_one() {
    let message = status_message(
        401,
        r#"{"errors":[{"type":"authentication_error","message":"OpenCode Go requires an API key"}]}"#,
        false,
    );
    assert!(message.starts_with("HTTP 401:"), "{message}");
    assert!(message.contains("requires an API key"), "{message}");
    assert!(message.contains("no API key was sent"), "{message}");
    for name in [
        "OPENCODE_GO_API_KEY",
        "OPENCODE_ZEN_API_KEY",
        "OPENCODE_API_KEY",
        "[ai].api_key",
    ] {
        assert!(message.contains(name), "{message}");
    }
}

#[test]
fn a_refused_key_is_not_reported_as_a_missing_one() {
    let message = status_message(403, "forbidden", true);
    assert_eq!(message, "HTTP 403: forbidden");
    // The hint is only about the key being absent, so it cannot hide a key the gateway rejected.
    assert!(!message.contains("no API key"));
}

#[test]
fn other_statuses_read_plainly() {
    assert_eq!(status_message(502, "", false), "HTTP 502");
    assert_eq!(
        status_message(502, "bad gateway\n", false),
        "HTTP 502: bad gateway"
    );
    // An endpoint that reported its own sentence keeps it, with no invented status in front.
    assert_eq!(
        status_message(0, "the mind map is empty", false),
        "the mind map is empty"
    );
    assert_eq!(status_message(200, "", true), "the AI request failed");
}

// ---- one conversation per board ------------------------------------------------------------------

fn chat_with(board: &str) -> sgraffito::ai::Chat {
    let mut chat = sgraffito::ai::Chat::new();
    chat.enter(board);
    chat
}

#[test]
fn every_board_keeps_its_own_transcript_and_draft() {
    let mut chat = chat_with("office");
    chat.session_mut()
        .push(sgraffito::ai::Role::User, "robot".into(), false);
    chat.session_mut().input = sgraffito::canvas::TextBuffer::new("half-typed");

    assert!(chat.enter("home"));
    assert!(
        chat.session().messages.is_empty(),
        "home inherited office's chat"
    );
    assert!(
        chat.session().input.text.is_empty(),
        "home inherited the draft"
    );
    chat.session_mut()
        .push(sgraffito::ai::Role::User, "car".into(), false);

    assert!(chat.enter("office"));
    assert_eq!(chat.session().messages.len(), 1);
    assert_eq!(chat.session().messages[0].text, "robot");
    assert_eq!(chat.session().input.text, "half-typed");
}

#[test]
fn a_failure_on_one_board_does_not_show_on_another() {
    let mut chat = chat_with("office");
    chat.fail("home", "HTTP 401");
    assert!(chat.session().error.is_none(), "office shows home's error");
    assert!(chat.enter("home"));
    assert_eq!(chat.session().error.as_deref(), Some("HTTP 401"));
}

#[test]
fn a_deleted_board_takes_its_conversation_with_it() {
    let mut chat = chat_with("office");
    chat.session_mut()
        .push(sgraffito::ai::Role::User, "robot".into(), false);
    chat.forget("office");
    assert!(
        chat.session().messages.is_empty(),
        "the transcript outlived its board"
    );
}

#[test]
fn entering_the_same_board_does_not_reset_it() {
    let mut chat = chat_with("office");
    chat.session_mut()
        .push(sgraffito::ai::Role::User, "robot".into(), false);
    assert!(!chat.enter("office"));
    assert_eq!(chat.session().messages.len(), 1);
}

// ---- the arrangement of a map --------------------------------------------------------------------

fn levels(depth: usize) -> MindNode {
    if depth == 0 {
        return leaf("x");
    }
    tree("root", vec![levels(depth - 1)])
}

#[test]
fn a_directive_names_the_arrangement_and_anything_else_is_ignored() {
    assert_eq!(
        layout_directive("mindmap\n  %% sgraffito layout: tree\n  root\n"),
        Some(MapKind::Tree)
    );
    assert_eq!(
        layout_directive("mindmap\n  %% sg\n  root"),
        None,
        "an unrelated comment is not a directive"
    );
    assert_eq!(
        layout_directive("mindmap\n  %% sgraffito layout: Tree\n  root"),
        Some(MapKind::Tree),
        "the name is matched without case"
    );
    assert_eq!(
        layout_directive("mindmap\n  %% sgraffito layout: spiral\n  root"),
        None,
        "an unknown arrangement is not silently accepted"
    );
    assert_eq!(layout_directive("mindmap\n  root"), None);
    for kind in [
        MapKind::Radial,
        MapKind::Tree,
        MapKind::Fishbone,
        MapKind::Compare,
    ] {
        assert_eq!(MapKind::from_name(kind.name()), Some(kind));
    }
}

#[test]
fn a_shallow_map_is_a_ring_and_a_hierarchy_is_a_tree() {
    assert_eq!(
        inferred_kind(&tree("root", vec![leaf("a"), leaf("b")])),
        MapKind::Radial
    );
    assert_eq!(
        inferred_kind(&tree("root", vec![tree("a", vec![leaf("a1")])])),
        MapKind::Tree,
        "three levels is a decomposition"
    );
    assert_eq!(inferred_kind(&levels(4)), MapKind::Tree);
}

#[test]
fn an_arrangement_that_cannot_be_drawn_falls_back_to_the_ring() {
    let one_child = tree("root", vec![leaf("only")]);
    assert_eq!(fit_kind(MapKind::Compare, &one_child), MapKind::Radial);
    let three_children = tree("root", vec![leaf("a"), leaf("b"), leaf("c")]);
    assert_eq!(fit_kind(MapKind::Compare, &three_children), MapKind::Radial);
    let two_children = tree("root", vec![leaf("a"), leaf("b")]);
    assert_eq!(fit_kind(MapKind::Compare, &two_children), MapKind::Compare);
    assert_eq!(
        fit_kind(MapKind::Fishbone, &leaf("root")),
        MapKind::Radial,
        "a fishbone needs a cause to hang on its spine"
    );
    assert_eq!(
        fit_kind(MapKind::Fishbone, &two_children),
        MapKind::Fishbone
    );
}

#[test]
fn a_tree_reads_left_to_right_with_one_column_per_level() {
    let map = tree(
        "root",
        vec![
            tree("parts", vec![leaf("a"), leaf("b")]),
            tree("assembly", vec![leaf("c")]),
        ],
    );
    let board = layout_mindmap(
        MapKind::Tree,
        &map,
        [100.0, 300.0],
        400.0,
        "#e01b24",
        &mut measure_fn(),
    );
    // Nodes are in the map's own ink; a branch's boundary is the same ink at a third of its strength.
    let boxes: Vec<&sgraffito::canvas::Shape> = board
        .shapes
        .iter()
        .filter(|shape| shape.kind == ShapeKind::Rect && shape.color == "#e01b24")
        .collect();
    assert_eq!(boxes.len(), 6);
    let groups = board
        .shapes
        .iter()
        .filter(|shape| {
            shape.kind == ShapeKind::Rect && shape.color.len() == 9 && shape.color.ends_with("88")
        })
        .count();
    assert_eq!(groups, 2, "one boundary per branch that has children");
    let root = boxes.iter().find(|shape| shape.id == 1).unwrap();
    // Every link goes rightward: no arrangement in this map may point back at the root.
    for line in board
        .shapes
        .iter()
        .filter(|shape| shape.kind == ShapeKind::Line)
    {
        assert!(
            line.to[0] > line.from[0],
            "a tree edge runs leftwards: {:?}",
            (line.from, line.to)
        );
    }
    // Children of one level share a column, and columns are ordered by depth.
    let mut columns: Vec<f32> = boxes.iter().map(|shape| shape.from[0]).collect();
    columns.sort_by(|a, b| a.partial_cmp(b).unwrap());
    columns.dedup_by(|a, b| (*a - *b).abs() < 1.0);
    assert_eq!(columns.len(), 3, "one column per level: {columns:?}");
    assert!(root.from[0] < columns[1], "the root is the leftmost column");
    // The room's left edge is where a rightward map starts, not its middle: `[100, 300]` is the left
    // edge of a 400-wide room, so the root's box begins just inside it.
    assert!(
        (root.from[0] - 124.0).abs() < 0.5,
        "the tree did not start at the left edge of its room: {}",
        root.from[0]
    );
}

#[test]
fn a_fishbone_hangs_causes_on_both_sides_of_one_spine() {
    let map = tree(
        "effect",
        vec![
            tree("people", vec![leaf("training")]),
            leaf("process"),
            tree("tools", vec![leaf("licences")]),
        ],
    );
    let board = layout_mindmap(
        MapKind::Fishbone,
        &map,
        [80.0, 300.0],
        400.0,
        "#33d17a",
        &mut measure_fn(),
    );
    let lines: Vec<&sgraffito::canvas::Shape> = board
        .shapes
        .iter()
        .filter(|shape| shape.kind == ShapeKind::Line)
        .collect();
    // The spine, one bone per cause, and one line per grandchild.
    assert_eq!(lines.len(), 1 + 3 + 2, "{lines:?}");
    let spine = lines
        .iter()
        .find(|line| line.attach[1].is_none())
        .expect("a spine has one free end");
    assert!((spine.from[1] - 300.0).abs() < 0.5 && (spine.to[1] - 300.0).abs() < 0.5);
    let causes: Vec<&sgraffito::canvas::Shape> = board
        .shapes
        .iter()
        .filter(|shape| shape.kind == ShapeKind::Rect)
        .collect();
    let above = causes
        .iter()
        .filter(|shape| shape.centre()[1] < 300.0)
        .count();
    let below = causes
        .iter()
        .filter(|shape| shape.centre()[1] > 300.0)
        .count();
    assert!(
        above >= 1 && below >= 1,
        "every cause is on one side of the spine: {above}/{below}"
    );
    // Every label is horizontal: a box's text is centred and no shape is turned.
    for shape in &board.shapes {
        assert_eq!(shape.rotate, 0.0, "a label or box was turned");
    }
}

#[test]
fn a_comparison_puts_what_two_subjects_share_between_them() {
    let map = tree(
        "Robot vs car",
        vec![
            tree("Robot", vec![leaf("Camera"), leaf("Wheels")]),
            tree("Car", vec![leaf("Engine"), leaf("Wheels")]),
        ],
    );
    let board = layout_mindmap(
        MapKind::Compare,
        &map,
        [400.0, 300.0],
        400.0,
        "#3584e4",
        &mut measure_fn(),
    );
    let box_of = |text: &str| -> (f32, f32) {
        let label = board
            .texts
            .iter()
            .find(|item| item.text == text)
            .unwrap_or_else(|| panic!("no label {text}"));
        let shape = board
            .shapes
            .iter()
            .find(|shape| Some(shape.id) == label.container)
            .unwrap();
        let centre = shape.centre();
        (centre[0], centre[1])
    };
    let (left, _) = box_of("Robot");
    let (right, _) = box_of("Car");
    let (shared, _) = box_of("Wheels");
    assert!(left < shared && shared < right, "{left} {shared} {right}");
    // What only one subject has stays in that subject's column; only the shared attribute is between.
    assert!(
        (box_of("Camera").0 - left).abs() < 1.0,
        "Camera left its subject's column"
    );
    assert!(
        (box_of("Engine").0 - right).abs() < 1.0,
        "Engine left its subject's column"
    );
    // The shared attribute hangs from both subjects, so it is drawn twice.
    let shared_id = board
        .texts
        .iter()
        .find(|item| item.text == "Wheels")
        .and_then(|item| item.container)
        .unwrap();
    let into_shared = board
        .shapes
        .iter()
        .filter(|shape| {
            shape.kind == ShapeKind::Line && shape.attach[1].is_some_and(|a| a.shape == shared_id)
        })
        .count();
    assert_eq!(
        into_shared, 2,
        "the shared node is not joined to both subjects"
    );
}

// ---- scrolling the transcript -------------------------------------------------------------------

#[test]
fn a_burst_accelerates_a_wheel_and_caps_out() {
    use sgraffito::ai::{BURST_MAX, Burst, SCROLL_PIXELS};
    use std::time::{Duration, Instant};
    let start = Instant::now();
    let mut burst = Burst::default();
    // Notches 50 ms apart are one burst: one, then half again, then twice, and no further than four.
    let mut factors = Vec::new();
    for step in 0..8 {
        let at = start + Duration::from_millis(50 * step);
        factors.push(burst.take(at, true));
    }
    let expected = [1.0, 1.5, 2.0, 2.5, 3.0, 3.5, 4.0, BURST_MAX];
    assert_eq!(factors, expected, "{factors:?}");
    const { assert!(BURST_MAX <= 4.0) };
    const { assert!(SCROLL_PIXELS > 0.0) };
}

#[test]
fn a_pause_ends_the_burst_and_a_touchpad_counts_gestures() {
    use sgraffito::ai::Burst;
    use std::time::{Duration, Instant};
    let start = Instant::now();
    let mut burst = Burst::default();
    assert_eq!(burst.take(start, true), 1.0);
    // A deliberate notch 300 ms later is a new burst, not the second step of one.
    assert_eq!(burst.take(start + Duration::from_millis(300), true), 1.0);
    // Inside one gesture a touchpad's events are one step: they are already continuous.
    assert_eq!(burst.take(start + Duration::from_millis(310), false), 1.0);
    assert_eq!(burst.take(start + Duration::from_millis(320), false), 1.0);
    // A second flick, separated by the gesture's break, is the next step.
    assert_eq!(burst.take(start + Duration::from_millis(600), false), 1.5);
    // The acceleration ends 400 ms after the last step; the number stays long enough to read it.
    let last = start + Duration::from_millis(600);
    assert_eq!(burst.factor(last + Duration::from_millis(300)), 1.5);
    assert_eq!(burst.factor(last + Duration::from_millis(400)), 1.0);
    assert_eq!(burst.label(last + Duration::from_millis(500)), 1.5);
    assert_eq!(burst.label(last + Duration::from_millis(1000)), 1.0);
    assert!(burst.hold_left(last + Duration::from_millis(500)).is_some());
    assert!(
        burst
            .hold_left(last + Duration::from_millis(1000))
            .is_none()
    );
}

#[test]
fn the_burst_label_reads_halves_and_whole_numbers() {
    assert_eq!(sgraffito::ai::burst_label(1.0), "1x");
    assert_eq!(sgraffito::ai::burst_label(1.5), "1.5x");
    assert_eq!(sgraffito::ai::burst_label(2.0), "2x");
    assert_eq!(sgraffito::ai::burst_label(4.0), "4x");
}

#[test]
fn a_wheel_moves_the_target_and_the_drawing_eases_toward_it() {
    use sgraffito::ai::Session;
    use std::time::Instant;
    let start = Instant::now();
    let mut session = Session::new();
    session.scroll_by(48.0, true, start, 1000.0);
    assert_eq!(session.scroll_target, 48.0);
    // The drawn offset is still at the top until the frames run, which is what makes it glide.
    assert_eq!(session.scroll, 0.0);
    assert!(session.scrolling());

    let mut frames = 0;
    while session.scrolling() && frames < 60 {
        session.ease_scroll(1.0 / 60.0);
        frames += 1;
    }
    assert!(frames < 30, "the scroll took {frames} frames");
    assert_eq!(session.scroll, session.scroll_target);

    // Past the end the wheel clamps, and following stops at the last step.
    session.scroll_by(10_000.0, true, start, 1000.0);
    assert_eq!(session.scroll_target, 1000.0);
    assert!(session.follow);
    session.clamp_scroll(1000.0);
    assert_eq!(session.scroll_target, 1000.0);
    session.follow = false;
    session.scroll_target = 400.0;
    session.clamp_scroll(300.0);
    assert_eq!(session.scroll_target, 300.0);
    assert_eq!(
        session.scroll, 48.0,
        "a clamped target does not jump the drawing"
    );
}

#[test]
fn a_burst_moves_further_than_the_notch_that_started_it() {
    use sgraffito::ai::Session;
    use std::time::{Duration, Instant};
    let start = Instant::now();
    let mut session = Session::new();
    session.scroll_by(10.0, true, start, 1000.0);
    assert_eq!(session.scroll_target, 10.0);
    session.scroll_by(10.0, true, start + Duration::from_millis(40), 1000.0);
    assert_eq!(
        session.scroll_target, 25.0,
        "the second notch is half again"
    );
}
