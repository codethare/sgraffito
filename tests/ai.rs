//! The AI panel's non-UI half: endpoint resolution, the Mermaid slice it understands, and the layout it
//! commits. Nothing here needs a compositor, a network or a key.

use std::collections::HashMap;
use std::time::Duration;

use sgraffito::ai::{
    AiSettings, MAX_MINDMAP_NODES, MindNode, extract_mermaid, layout_mindmap, parse_mindmap,
    resolve_with,
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
        ai.endpoint,
        "https://gateway.example/v1/openai/v1/chat/completions"
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
    let board = layout_mindmap(&map, [400.0, 300.0], "#e01b24", &mut measure_fn());
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
    let board = layout_mindmap(&map, [0.0, 0.0], "#3584e4", &mut measure_fn());
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
    let board = layout_mindmap(&map, [100.0, 50.0], "#f6d32d", &mut measure_fn());
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
    let board = layout_mindmap(&map, [250.0, 175.0], "#33d17a", &mut measure_fn());
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
    let board = layout_mindmap(&leaf("alone"), [0.0, 0.0], "#ffffff", &mut measure_fn());
    assert_eq!(board.shapes.len(), 1);
    assert_eq!(board.texts.len(), 1);
    assert_eq!(board.texts[0].container, Some(1));
}
