//! Optional configuration: `$XDG_CONFIG_HOME/sgraffito/config.toml`, falling back to
//! `$HOME/.config/sgraffito/config.toml`.
//!
//! Every value is optional. A missing file, an unreadable one, a malformed one or an invalid value
//! leaves the built-in default in place and logs why, because a typo in a preference must not stop the
//! daemon from running.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;

/// Sticky-note border thickness when the file does not say, in logical pixels.
pub const DEFAULT_BORDER: f32 = 1.0;
/// The built-in five colours (`#rrggbb`), which are also the fallback for every invalid entry.
pub const DEFAULT_COLORS: [&str; COLOR_COUNT] = crate::canvas::PALETTE;
pub const COLOR_COUNT: usize = 5;

/// Which screen edge the edit-mode toolbar is docked to. The rail is a column on the left and
/// right edges and a row on the top and bottom ones, and its panels open toward the interior.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ToolbarPosition {
    #[default]
    Left,
    Right,
    Top,
    Bottom,
}

impl ToolbarPosition {
    /// The value the configuration file writes.
    pub const fn name(self) -> &'static str {
        match self {
            ToolbarPosition::Left => "left",
            ToolbarPosition::Right => "right",
            ToolbarPosition::Top => "top",
            ToolbarPosition::Bottom => "bottom",
        }
    }

    /// Whether the rail runs left to right along the top or bottom edge. The blocks are still
    /// upright; only the axis they are laid out on changes.
    pub const fn horizontal(self) -> bool {
        matches!(self, ToolbarPosition::Top | ToolbarPosition::Bottom)
    }
}

/// Parse a toolbar edge. The file's value is matched case-insensitively and without surrounding
/// whitespace; anything else is invalid.
pub fn parse_toolbar_position(input: &str) -> Option<ToolbarPosition> {
    match input.trim().to_ascii_lowercase().as_str() {
        "left" => Some(ToolbarPosition::Left),
        "right" => Some(ToolbarPosition::Right),
        "top" => Some(ToolbarPosition::Top),
        "bottom" => Some(ToolbarPosition::Bottom),
        _ => None,
    }
}

/// The values the daemon reads once at startup.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Sticky-note border thickness, in logical pixels. Zero draws no border.
    pub border: f32,
    /// The five palette colours, normalized to `#rrggbb` or `#rrggbbaa`.
    pub colors: [String; COLOR_COUNT],
    /// The screen edge the edit-mode toolbar is docked to.
    pub toolbar: ToolbarPosition,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            border: DEFAULT_BORDER,
            colors: DEFAULT_COLORS.map(str::to_string),
            toolbar: ToolbarPosition::default(),
        }
    }
}

/// Loaded once by the daemon. The default is kept apart from the load so a test that renders a frame
/// never picks up the developer's own `config.toml`.
static CONFIG: OnceLock<Config> = OnceLock::new();

/// The configuration for this process: the file loaded by [`init`], or the defaults when the daemon
/// has not started. Rendering and drawing both read this.
pub fn current() -> &'static Config {
    CONFIG.get_or_init(Config::default)
}

/// Read the file once and make it current. A later call does nothing: configuration is startup state.
pub fn init() -> &'static Config {
    CONFIG.get_or_init(load)
}

/// `$XDG_CONFIG_HOME/sgraffito/config.toml`, falling back to `$HOME/.config`.
pub fn path_in(xdg_config_home: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    let base = match xdg_config_home {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(home?).join(".config"),
    };
    Some(base.join("sgraffito/config.toml"))
}

pub fn path() -> Option<PathBuf> {
    path_in(
        std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

fn load() -> Config {
    match path() {
        Some(path) => load_from(&path),
        None => {
            crate::log::write("config: neither $XDG_CONFIG_HOME nor $HOME is set; using defaults");
            Config::default()
        }
    }
}

/// Read `path`, keeping the default for anything that is missing or invalid. A missing file is not
/// worth a word: configuration is optional.
pub fn load_from(path: &Path) -> Config {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Config::default(),
        Err(e) => {
            crate::log::write(&format!(
                "config: cannot read {}: {e}; using defaults",
                path.display()
            ));
            return Config::default();
        }
    };
    let file: File = match toml::from_str(&raw) {
        Ok(file) => file,
        Err(e) => {
            crate::log::write(&format!(
                "config: {} is not valid TOML: {e}; using defaults",
                path.display()
            ));
            return Config::default();
        }
    };

    let mut config = Config::default();
    if let Some(border) = file.border {
        let border = border.value() as f32;
        if border.is_finite() && border >= 0.0 {
            config.border = border;
        } else {
            crate::log::write(&format!(
                "config: border must be a finite, non-negative number; using {DEFAULT_BORDER}"
            ));
        }
    }
    if let Some(colors) = file.colors {
        if colors.len() != COLOR_COUNT {
            crate::log::write(&format!(
                "config: colors takes exactly {COLOR_COUNT} entries, not {}; keeping the defaults",
                colors.len()
            ));
        } else {
            for (index, raw) in colors.iter().enumerate() {
                match parse_color(raw) {
                    Some(color) => config.colors[index] = color,
                    None => crate::log::write(&format!(
                        "config: colour {} `{raw}` is not hex, rgb/rgba or hsl/hsla; using {}",
                        index + 1,
                        config.colors[index]
                    )),
                }
            }
        }
    }
    if let Some(toolbar) = file.toolbar {
        match parse_toolbar_position(&toolbar) {
            Some(position) => config.toolbar = position,
            None => crate::log::write(&format!(
                "config: toolbar must be left, right, top or bottom, not `{toolbar}`; using {}",
                config.toolbar.name()
            )),
        }
    }
    config
}

#[derive(Deserialize)]
struct File {
    #[serde(default)]
    border: Option<Number>,
    #[serde(default)]
    colors: Option<Vec<String>>,
    #[serde(default)]
    toolbar: Option<String>,
}

/// TOML has separate integer and float types, and `border = 2` must mean the same as `2.0`.
#[derive(Deserialize)]
#[serde(untagged)]
enum Number {
    Integer(i64),
    Float(f64),
}

impl Number {
    fn value(&self) -> f64 {
        match self {
            Number::Integer(n) => *n as f64,
            Number::Float(n) => *n,
        }
    }
}

/// Parse one colour and normalize it to `#rrggbb` or `#rrggbbaa`. Accepts the short and long hex
/// forms, `rgb()`/`rgba()` with `,` or spaces, and `hsl()`/`hsla()` with a `deg`/`grad`/`rad`/`turn`
/// or bare hue. Numbers may carry `%` where CSS allows one.
pub fn parse_color(input: &str) -> Option<String> {
    let text = input.trim();
    if let Some(hex) = text.strip_prefix('#') {
        return parse_hex(hex);
    }
    let lower = text.to_ascii_lowercase();
    let (name, rest) = lower.split_once('(')?;
    let body = rest.strip_suffix(')')?;
    match name.trim() {
        "rgb" | "rgba" => parse_rgb(body),
        "hsl" | "hsla" => parse_hsl(body),
        _ => None,
    }
}

fn parse_hex(hex: &str) -> Option<String> {
    if hex.is_empty() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let digits: Vec<u32> = hex.chars().map(|c| c.to_digit(16)).collect::<Option<_>>()?;
    let (r, g, b, a) = match digits.len() {
        3 => (
            digits[0] as u8 * 17,
            digits[1] as u8 * 17,
            digits[2] as u8 * 17,
            255,
        ),
        4 => (
            digits[0] as u8 * 17,
            digits[1] as u8 * 17,
            digits[2] as u8 * 17,
            digits[3] as u8 * 17,
        ),
        6 => (
            (digits[0] * 16 + digits[1]) as u8,
            (digits[2] * 16 + digits[3]) as u8,
            (digits[4] * 16 + digits[5]) as u8,
            255,
        ),
        8 => (
            (digits[0] * 16 + digits[1]) as u8,
            (digits[2] * 16 + digits[3]) as u8,
            (digits[4] * 16 + digits[5]) as u8,
            (digits[6] * 16 + digits[7]) as u8,
        ),
        _ => return None,
    };
    Some(format_color([r, g, b, a]))
}

fn parse_rgb(body: &str) -> Option<String> {
    let (channels, alpha) = split_args(body)?;
    let (r, g, b) = match channels.as_slice() {
        [r, g, b] => (channel(r)?, channel(g)?, channel(b)?),
        _ => return None,
    };
    Some(format_color([r, g, b, alpha.unwrap_or(255)]))
}

fn parse_hsl(body: &str) -> Option<String> {
    let (channels, alpha) = split_args(body)?;
    let (h, s, l) = match channels.as_slice() {
        [h, s, l] => (hue(h)?, percentage(s)?, percentage(l)?),
        _ => return None,
    };
    let [r, g, b] = hsl_to_rgb(h, s, l);
    Some(format_color([r, g, b, alpha.unwrap_or(255)]))
}

/// Split a colour function's body into its three channels and an optional alpha, with `rgba(1, 2, 3,
/// 0.5)`, `rgb(1 2 3 / 0.5)` and `rgb(1 2 3)` all accepted.
fn split_args(body: &str) -> Option<(Vec<&str>, Option<u8>)> {
    let (main, slash) = match body.split_once('/') {
        Some((main, alpha)) => (main, Some(alpha.trim())),
        None => (body, None),
    };
    let mut channels: Vec<&str> = main
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|part| !part.is_empty())
        .collect();
    let mut alpha = match slash {
        Some(slash) => Some(alpha_channel(slash)?),
        None => None,
    };
    if alpha.is_none() && channels.len() == 4 {
        alpha = Some(alpha_channel(channels.pop()?)?);
    }
    (channels.len() == 3).then_some((channels, alpha))
}

/// One `rgb()` channel: `0..=255`, or a percentage of the same range.
fn channel(token: &str) -> Option<u8> {
    let token = token.trim();
    let value = if let Some(percent) = token.strip_suffix('%') {
        let percent = parse_finite(percent)?;
        if !(0.0..=100.0).contains(&percent) {
            return None;
        }
        percent / 100.0 * 255.0
    } else {
        let value = parse_finite(token)?;
        if !(0.0..=255.0).contains(&value) {
            return None;
        }
        value
    };
    Some(value.round() as u8)
}

/// An alpha value: `0..=1`, or a percentage.
fn alpha_channel(token: &str) -> Option<u8> {
    let token = token.trim();
    let value = if let Some(percent) = token.strip_suffix('%') {
        let percent = parse_finite(percent)?;
        if !(0.0..=100.0).contains(&percent) {
            return None;
        }
        percent / 100.0
    } else {
        let value = parse_finite(token)?;
        if !(0.0..=1.0).contains(&value) {
            return None;
        }
        value
    };
    Some((value * 255.0).round() as u8)
}

/// A saturation or lightness channel: a percentage, as CSS requires for `hsl()`.
fn percentage(token: &str) -> Option<f64> {
    let token = token.trim();
    let percent = token.strip_suffix('%')?;
    let percent = parse_finite(percent)?;
    (0.0..=100.0).contains(&percent).then_some(percent)
}

/// A hue in degrees, `deg`, `grad`, `rad` or `turn`.
fn hue(token: &str) -> Option<f64> {
    let token = token.trim();
    let (number, scale) = if let Some(deg) = token.strip_suffix("deg") {
        (deg, 1.0)
    } else if let Some(grad) = token.strip_suffix("grad") {
        (grad, 0.9)
    } else if let Some(rad) = token.strip_suffix("rad") {
        (rad, 180.0 / std::f64::consts::PI)
    } else if let Some(turn) = token.strip_suffix("turn") {
        (turn, 360.0)
    } else {
        (token, 1.0)
    };
    Some(parse_finite(number)? * scale).map(|hue| hue.rem_euclid(360.0))
}

fn parse_finite(token: &str) -> Option<f64> {
    let value: f64 = token.trim().parse().ok()?;
    value.is_finite().then_some(value)
}

/// `h` in degrees, `s` and `l` in percent.
fn hsl_to_rgb(h: f64, s: f64, l: f64) -> [u8; 3] {
    let (s, l) = (s / 100.0, l / 100.0);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - (h / 60.0 % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match h / 60.0 {
        h if h < 1.0 => (c, x, 0.0),
        h if h < 2.0 => (x, c, 0.0),
        h if h < 3.0 => (0.0, c, x),
        h if h < 4.0 => (0.0, x, c),
        h if h < 5.0 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [(r + m) * 255.0, (g + m) * 255.0, (b + m) * 255.0].map(|v| v.round() as u8)
}

fn format_color([r, g, b, a]: [u8; 4]) -> String {
    if a == 255 {
        format!("#{r:02x}{g:02x}{b:02x}")
    } else {
        format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_forms_normalize_to_the_long_form() {
        assert_eq!(parse_color("#e01b24").as_deref(), Some("#e01b24"));
        assert_eq!(parse_color("#F80").as_deref(), Some("#ff8800"));
        assert_eq!(parse_color("#ffff").as_deref(), Some("#ffffff"));
        assert_eq!(parse_color("#3584e480").as_deref(), Some("#3584e480"));
        assert_eq!(parse_color("#xyz"), None);
        assert_eq!(parse_color("#12345"), None);
    }

    #[test]
    fn rgb_and_rgba_accept_both_separators_and_percentages() {
        assert_eq!(parse_color("rgb(224, 27, 36)").as_deref(), Some("#e01b24"));
        assert_eq!(parse_color("rgb(224 27 36)").as_deref(), Some("#e01b24"));
        assert_eq!(
            parse_color("rgba(224, 27, 36, 0.5)").as_deref(),
            Some("#e01b2480")
        );
        assert_eq!(
            parse_color("rgb(100% 0% 0% / 50%)").as_deref(),
            Some("#ff000080")
        );
        assert_eq!(parse_color("rgb(256, 0, 0)"), None);
        assert_eq!(parse_color("rgba(1, 2, 3, 2)"), None);
    }

    #[test]
    fn hsl_forms_convert_and_wrap_the_hue() {
        assert_eq!(parse_color("hsl(0, 100%, 50%)").as_deref(), Some("#ff0000"));
        assert_eq!(
            parse_color("hsl(120deg, 50%, 50%)").as_deref(),
            Some("#40bf40")
        );
        assert_eq!(
            parse_color("hsla(360, 100%, 50%, 0.5)").as_deref(),
            Some("#ff000080")
        );
        assert_eq!(
            parse_color("hsl(0.5turn 100% 50%)").as_deref(),
            Some("#00ffff")
        );
        assert_eq!(parse_color("hsl(0, 100, 50)"), None);
    }

    #[test]
    fn another_function_name_is_not_a_colour() {
        assert_eq!(parse_color("lab(50% 20 30)"), None);
        assert_eq!(parse_color("red"), None);
    }

    #[test]
    fn the_path_honours_xdg_and_falls_back_to_home() {
        assert_eq!(
            path_in(Some("/xdg"), Some("/home/u")).as_deref(),
            Some(Path::new("/xdg/sgraffito/config.toml"))
        );
        assert_eq!(
            path_in(None, Some("/home/u")).as_deref(),
            Some(Path::new("/home/u/.config/sgraffito/config.toml"))
        );
        assert_eq!(
            path_in(Some(""), Some("/home/u")).as_deref(),
            Some(Path::new("/home/u/.config/sgraffito/config.toml"))
        );
        assert_eq!(path_in(None, None), None);
    }

    fn config_in(dir: &Path, body: &str) -> Config {
        let path = dir.join("config.toml");
        fs::write(&path, body).unwrap();
        load_from(&path)
    }

    #[test]
    fn a_missing_file_is_the_default() {
        let dir = std::env::temp_dir().join(format!("sgraffito-config-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(load_from(&dir.join("absent.toml")), Config::default());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn values_are_read_and_invalid_ones_fall_back() {
        let dir =
            std::env::temp_dir().join(format!("sgraffito-config-values-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let config = config_in(
            &dir,
            "border = 2\ncolors = [\"#123\", \"rgb(1 2 3)\", \"hsl(120, 100%, 25%)\", \"nope\", \"#ffffff80\"]\n",
        );
        assert_eq!(config.border, 2.0);
        assert_eq!(
            config.colors,
            [
                "#112233".to_string(),
                "#010203".to_string(),
                "#008000".to_string(),
                DEFAULT_COLORS[3].to_string(),
                "#ffffff80".to_string(),
            ]
        );
        let wrong_count = config_in(&dir, "colors = [\"#000000\"]\n");
        assert_eq!(wrong_count.colors, Config::default().colors);
        let negative = config_in(&dir, "border = -1\n");
        assert_eq!(negative.border, DEFAULT_BORDER);
        let malformed = config_in(&dir, "border = \n");
        assert_eq!(malformed, Config::default());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_toolbar_edge_is_case_insensitive_and_keeps_left_when_invalid() {
        let dir =
            std::env::temp_dir().join(format!("sgraffito-config-toolbar-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for (value, expected) in [
            ("left", ToolbarPosition::Left),
            ("Right", ToolbarPosition::Right),
            (" TOP ", ToolbarPosition::Top),
            ("bOtToM", ToolbarPosition::Bottom),
        ] {
            let config = config_in(&dir, &format!("toolbar = \"{value}\"\n"));
            assert_eq!(config.toolbar, expected, "toolbar = {value:?}");
        }
        assert_eq!(
            config_in(&dir, "toolbar = \"diagonal\"\n").toolbar,
            ToolbarPosition::Left,
            "an invalid edge must keep the built-in default"
        );
        assert_eq!(
            config_in(&dir, "").toolbar,
            ToolbarPosition::Left,
            "an absent edge must keep the built-in default"
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
