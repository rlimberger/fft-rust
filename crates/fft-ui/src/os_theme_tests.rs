//! Unit tests for `os_theme` (kept separate so the module stays under ~500 lines).

use super::*;
use std::io::Write;

// `r##` so embedded `"#rrggbb"` does not terminate the raw string.
const TOKYO_NIGHT: &str = r##"
mode = "dark"
accent = "#7aa2f7"
selection = "#292e42"
muted = "#414868"
background = "#1a1b26"
dark_background = "#13141c"
darker_background = "#0e0e14"
lighter_background = "#24283b"
foreground = "#a9b1d6"
dark_foreground = "#565f89"
light_foreground = "#b4bee6"
bright_foreground = "#c0caf5"
red = "#f7768e"
yellow = "#e0af68"
orange = "#eb927b"
green = "#9ece6a"
cyan = "#449dab"
blue = "#7aa2f7"
magenta = "#ad8ee6"
brown = "#75493d"
bright_red = "#ff7a93"
bright_yellow = "#ff9e64"
bright_green = "#b9f27c"
bright_cyan = "#0db9d7"
bright_blue = "#7da6ff"
bright_magenta = "#bb9af7"
"##;

const WHITE_NO_OPTIONAL: &str = r##"
mode = "light"
accent = "#6e6e6e"
selection = "#c0c0c0"
muted = "#808080"
background = "#ffffff"
dark_background = "#f5f5f5"
darker_background = "#e8e8e8"
lighter_background = "#c0c0c0"
foreground = "#000000"
dark_foreground = "#c0c0c0"
light_foreground = "#000000"
bright_foreground = "#000000"
red = "#2a2a2a"
yellow = "#4a4a4a"
green = "#3a3a3a"
cyan = "#3e3e3e"
blue = "#1a1a1a"
magenta = "#2e2e2e"
bright_red = "#2a2a2a"
bright_yellow = "#4a4a4a"
bright_green = "#3a3a3a"
bright_cyan = "#3e3e3e"
bright_blue = "#1a1a1a"
bright_magenta = "#2e2e2e"
"##;

#[test]
fn parse_tokyo_night_round_trip() {
    let c = parse_colors_toml(TOKYO_NIGHT).expect("tokyo-night parses");
    assert_eq!(c.mode, "dark");
    assert_eq!(c.background, 0x1a1b26);
    assert_eq!(c.foreground, 0xa9b1d6);
    assert_eq!(c.orange, 0xeb927b);
    assert_eq!(c.brown, 0x75493d);
    assert_eq!(c.bright_cyan, 0x0db9d7);
    assert_eq!(c.bright_red, 0xff7a93);
}

#[test]
fn missing_required_key_is_err() {
    let mut text = TOKYO_NIGHT.to_string();
    text = text
        .lines()
        .filter(|l| !l.starts_with("background"))
        .collect::<Vec<_>>()
        .join("\n");
    let err = parse_colors_toml(&text).unwrap_err();
    assert!(err.contains("background"), "{err}");
}

#[test]
fn optional_orange_brown_fallback() {
    let c = parse_colors_toml(WHITE_NO_OPTIONAL).expect("white parses");
    assert_eq!(c.orange, c.bright_yellow);
    assert_eq!(c.brown, c.muted);
    assert_eq!(c.orange, 0x4a4a4a);
    assert_eq!(c.brown, 0x808080);
}

#[test]
fn bad_hex_is_err() {
    let text = TOKYO_NIGHT.replace("#1a1b26", "#gg0000");
    let err = parse_colors_toml(&text).unwrap_err();
    assert!(err.contains("background") || err.contains("hex"), "{err}");
}

#[test]
fn parse_hex_accepts_hash_and_bare() {
    assert_eq!(parse_hex("#1a1b26").unwrap(), 0x1a1b26);
    assert_eq!(parse_hex("1a1b26").unwrap(), 0x1a1b26);
    assert!(parse_hex("#abc").is_err());
    assert!(parse_hex("zzzzzz").is_err());
}

#[test]
fn base_size_from_font_section_only() {
    let text = r#"
[bar]
base-size = 99
[font]
base-size = 14
"#;
    assert_eq!(parse_base_size_from_shell_toml(text).unwrap(), Some(14.0));
    assert_eq!(
        parse_base_size_from_shell_toml("[bar]\nbase-size = 9\n").unwrap(),
        None
    );
    assert_eq!(
        parse_base_size_from_shell_toml("[font]\nbase-size = 5.5\n").unwrap(),
        Some(6.0)
    );
}

#[test]
fn base_size_precedence_user_over_themed_over_default() {
    let dir = std::env::temp_dir().join(format!("fft-os-theme-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let user = dir.join("user-shell.toml");
    let themed = dir.join("themed-shell.toml");

    // Neither file → default 12.
    assert_eq!(load_base_size_from_paths(&user, &themed), 12.0);

    // Themed only.
    write_file(&themed, "[font]\nbase-size = 12\n");
    assert_eq!(load_base_size_from_paths(&user, &themed), 12.0);

    // User wins.
    write_file(&user, "[font]\nbase-size = 14\n");
    assert_eq!(load_base_size_from_paths(&user, &themed), 14.0);

    // Malformed user falls through to themed.
    write_file(&user, "[font]\nbase-size = nope\n");
    assert_eq!(load_base_size_from_paths(&user, &themed), 12.0);

    let _ = fs::remove_dir_all(&dir);
}

fn write_file(path: &Path, body: &str) {
    let mut f = fs::File::create(path).unwrap();
    f.write_all(body.as_bytes()).unwrap();
}
