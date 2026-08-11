//! Palette roles for every paint path.
//!
//! Production colors come from the host Omarchy theme via [`Palette::from_os_colors`].
//! [`Palette::mocha`] / [`Palette::latte`] remain as the documented CI fallback and
//! test fixtures (mocha is what the watcher publishes when Omarchy is absent).

use gpui::{Hsla, Rgba, rgb};

use crate::os_theme::OsColors;

/// Official role colors used by panes and chrome.
///
/// No `Option`s: every role is always present. Alpha is baked into the role
/// where the prior paint path used translucent quads (period cursor/gap,
/// semantic lines, current price, VA row tint).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub base: Hsla,
    pub mantle: Hsla,
    pub surface: Hsla,
    pub overlay: Hsla,
    pub text: Hsla,
    pub subtext: Hsla,
    pub footer_bg: Hsla,
    pub divider: Hsla,
    pub splitter: Hsla,
    pub va_bg: Hsla,
    pub vpoc: Hsla,
    pub ib: Hsla,
    pub vah_val: Hsla,
    pub current_price: Hsla,
    /// Session-open hairline (first print); quieter than `current_price`.
    pub session_open: Hsla,
    pub period_cursor: Hsla,
    pub period_gap: Hsla,
    pub eth_tpo: Hsla,
    pub rth_tpo: Hsla,
    pub pv_bar: Hsla,
    pub sv_total: Hsla,
    pub buy: Hsla,
    pub sell: Hsla,
    pub bid_depth: Hsla,
    pub ask_depth: Hsla,
    pub inside_band: Hsla,
    pub blank_window: Hsla,
}

impl Palette {
    /// Map Omarchy `colors.toml` roles onto FFT paint roles (orchestrator contract).
    pub fn from_os_colors(c: &OsColors) -> Self {
        Self {
            base: solid(c.background),
            mantle: solid(c.dark_background),
            footer_bg: solid(c.dark_background),
            blank_window: solid(c.darker_background),
            surface: solid(c.selection),
            inside_band: solid(c.selection),
            overlay: solid(c.muted),
            sv_total: solid(c.muted),
            text: solid(c.foreground),
            subtext: solid(c.dark_foreground),
            eth_tpo: solid(c.dark_foreground),
            divider: solid(c.selection),
            splitter: solid(c.lighter_background),
            pv_bar: solid(c.lighter_background),
            va_bg: alpha(c.selection, 0.55),
            vpoc: alpha(c.orange, 0.75),
            ib: alpha(c.yellow, 0.45),
            vah_val: alpha(c.cyan, 0.55),
            current_price: alpha(c.foreground, 0.80),
            session_open: alpha(c.magenta, 0.40),
            period_cursor: alpha(c.orange, 0.16),
            period_gap: alpha(c.red, 0.12),
            rth_tpo: solid(c.orange),
            bid_depth: solid(c.blue),
            ask_depth: solid(c.red),
            buy: solid(c.bright_cyan),
            sell: solid(c.bright_red),
        }
    }

    /// Catppuccin Mocha (default dark flavor; OS-theme fallback + test fixture).
    pub fn mocha() -> Self {
        // Official Mocha hex (catppuccin.com/palette).
        const PEACH: u32 = 0xfab387;
        const YELLOW: u32 = 0xf9e2af;
        const TEAL: u32 = 0x94e2d5;
        const LAVENDER: u32 = 0xb4befe;
        const SAPPHIRE: u32 = 0x74c7ec;
        const BLUE: u32 = 0x89b4fa;
        const RED: u32 = 0xf38ba8;
        const MAROON: u32 = 0xeba0ac;
        const TEXT: u32 = 0xcdd6f4;
        const SUBTEXT0: u32 = 0xa6adc8;
        const OVERLAY0: u32 = 0x6c7086;
        const OVERLAY1: u32 = 0x7f849c;
        const SURFACE0: u32 = 0x313244;
        const SURFACE1: u32 = 0x45475a;
        const SURFACE2: u32 = 0x585b70;
        const BASE: u32 = 0x1e1e2e;
        const MANTLE: u32 = 0x181825;
        const CRUST: u32 = 0x11111b;

        Self {
            base: solid(BASE),
            mantle: solid(MANTLE),
            surface: solid(SURFACE0),
            overlay: solid(OVERLAY0),
            text: solid(TEXT),
            subtext: solid(SUBTEXT0),
            footer_bg: solid(MANTLE),
            divider: solid(SURFACE1),
            splitter: solid(SURFACE2),
            va_bg: alpha(SURFACE0, 0.55),
            vpoc: alpha(PEACH, 0.75),
            ib: alpha(YELLOW, 0.45),
            vah_val: alpha(TEAL, 0.55),
            current_price: alpha(TEXT, 0.80),
            session_open: alpha(LAVENDER, 0.40),
            period_cursor: alpha(PEACH, 0.16),
            period_gap: alpha(RED, 0.12),
            eth_tpo: solid(OVERLAY1),
            rth_tpo: solid(PEACH),
            pv_bar: solid(SURFACE2),
            sv_total: solid(OVERLAY0),
            buy: solid(SAPPHIRE),
            sell: solid(MAROON),
            bid_depth: solid(BLUE),
            ask_depth: solid(RED),
            inside_band: solid(SURFACE0),
            blank_window: solid(CRUST),
        }
    }

    /// Catppuccin Latte (light flavor; test fixture).
    pub fn latte() -> Self {
        // Official Latte hex (catppuccin.com/palette).
        const PEACH: u32 = 0xfe640b;
        const YELLOW: u32 = 0xdf8e1d;
        const TEAL: u32 = 0x179299;
        const LAVENDER: u32 = 0x7287fd;
        const SAPPHIRE: u32 = 0x209fb5;
        const BLUE: u32 = 0x1e66f5;
        const RED: u32 = 0xd20f39;
        const MAROON: u32 = 0xe64553;
        const TEXT: u32 = 0x4c4f69;
        const SUBTEXT0: u32 = 0x6c6f85;
        const OVERLAY0: u32 = 0x9ca0b0;
        const OVERLAY1: u32 = 0x8c8fa1;
        const SURFACE0: u32 = 0xccd0da;
        const SURFACE1: u32 = 0xbcc0cc;
        const SURFACE2: u32 = 0xacb0be;
        const BASE: u32 = 0xeff1f5;
        const MANTLE: u32 = 0xe6e9ef;
        const CRUST: u32 = 0xdce0e8;

        Self {
            base: solid(BASE),
            mantle: solid(MANTLE),
            surface: solid(SURFACE0),
            overlay: solid(OVERLAY0),
            text: solid(TEXT),
            subtext: solid(SUBTEXT0),
            footer_bg: solid(MANTLE),
            divider: solid(SURFACE1),
            splitter: solid(SURFACE2),
            va_bg: alpha(SURFACE0, 0.55),
            vpoc: alpha(PEACH, 0.75),
            ib: alpha(YELLOW, 0.45),
            vah_val: alpha(TEAL, 0.55),
            current_price: alpha(TEXT, 0.80),
            session_open: alpha(LAVENDER, 0.40),
            period_cursor: alpha(PEACH, 0.16),
            period_gap: alpha(RED, 0.12),
            eth_tpo: solid(OVERLAY1),
            rth_tpo: solid(PEACH),
            pv_bar: solid(SURFACE2),
            sv_total: solid(OVERLAY0),
            buy: solid(SAPPHIRE),
            sell: solid(MAROON),
            bid_depth: solid(BLUE),
            ask_depth: solid(RED),
            inside_band: solid(SURFACE0),
            blank_window: solid(CRUST),
        }
    }
}

fn solid(hex: u32) -> Hsla {
    Hsla::from(rgb(hex))
}

fn alpha(hex: u32, a: f32) -> Hsla {
    let Rgba { r, g, b, .. } = rgb(hex);
    Hsla::from(Rgba { r, g, b, a })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::os_theme::parse_colors_toml;

    #[test]
    fn mocha_and_latte_are_complete_and_differ() {
        let mocha = Palette::mocha();
        let latte = Palette::latte();

        // Spot-check canonical roles against official hex (via rgb→Hsla).
        assert_eq!(mocha.base, solid(0x1e1e2e));
        assert_eq!(mocha.text, solid(0xcdd6f4));
        assert_eq!(mocha.bid_depth, solid(0x89b4fa));
        assert_eq!(mocha.ask_depth, solid(0xf38ba8));
        assert_eq!(mocha.vpoc.a, 0.75);
        assert_eq!(mocha.period_cursor.a, 0.16);
        assert_eq!(mocha.period_gap.a, 0.12);
        assert_eq!(mocha.current_price.a, 0.80);
        assert_eq!(mocha.session_open.a, 0.40);
        assert_eq!(mocha.ib.a, 0.45);
        assert_eq!(mocha.vah_val.a, 0.55);
        assert_eq!(mocha.va_bg.a, 0.55);
        assert_ne!(mocha.session_open, mocha.current_price);
        assert_ne!(mocha.session_open, mocha.vpoc);
        assert_ne!(mocha.session_open, mocha.vah_val);
        assert_ne!(mocha.session_open, mocha.ib);

        assert_eq!(latte.base, solid(0xeff1f5));
        assert_eq!(latte.text, solid(0x4c4f69));
        assert_eq!(latte.bid_depth, solid(0x1e66f5));
        assert_eq!(latte.ask_depth, solid(0xd20f39));

        assert_ne!(mocha.base, latte.base);
        assert_ne!(mocha.text, latte.text);
        assert_ne!(mocha.buy, latte.buy);
        assert_ne!(mocha.sell, latte.sell);
    }

    #[test]
    fn from_os_colors_tokyo_night_mapping() {
        let text = r##"
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
        let os = parse_colors_toml(text).expect("fixture");
        let p = Palette::from_os_colors(&os);
        assert_eq!(p.base, solid(0x1a1b26));
        assert_eq!(p.text, solid(0xa9b1d6));
        assert_eq!(p.mantle, solid(0x13141c));
        assert_eq!(p.footer_bg, solid(0x13141c));
        assert_eq!(p.blank_window, solid(0x0e0e14));
        assert_eq!(p.surface, solid(0x292e42));
        assert_eq!(p.inside_band, solid(0x292e42));
        assert_eq!(p.overlay, solid(0x414868));
        assert_eq!(p.sv_total, solid(0x414868));
        assert_eq!(p.subtext, solid(0x565f89));
        assert_eq!(p.eth_tpo, solid(0x565f89));
        assert_eq!(p.divider, solid(0x292e42));
        assert_eq!(p.splitter, solid(0x24283b));
        assert_eq!(p.pv_bar, solid(0x24283b));
        assert_eq!(p.rth_tpo, solid(0xeb927b));
        assert_eq!(p.bid_depth, solid(0x7aa2f7));
        assert_eq!(p.ask_depth, solid(0xf7768e));
        assert_eq!(p.buy, solid(0x0db9d7));
        assert_eq!(p.sell, solid(0xff7a93));
        assert!((p.va_bg.a - 0.55).abs() < 1e-5);
        assert!((p.vpoc.a - 0.75).abs() < 1e-5);
        assert!((p.ib.a - 0.45).abs() < 1e-5);
        assert!((p.vah_val.a - 0.55).abs() < 1e-5);
        assert!((p.current_price.a - 0.80).abs() < 1e-5);
        assert!((p.session_open.a - 0.40).abs() < 1e-5);
        assert!((p.period_cursor.a - 0.16).abs() < 1e-5);
        assert!((p.period_gap.a - 0.12).abs() < 1e-5);
        // Alpha roles share RGB with their solid source.
        assert_eq!(p.vpoc.h, solid(0xeb927b).h);
        assert_eq!(p.current_price.h, solid(0xa9b1d6).h);
        assert_eq!(p.session_open.h, solid(0xad8ee6).h);
    }
}
