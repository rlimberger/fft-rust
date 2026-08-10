//! Catppuccin palette roles for every paint path.
//!
//! Flavors are hand-written from the official Catppuccin hex values
//! (https://catppuccin.com/palette/). `FFT_THEME=latte` selects Latte; anything
//! else or unset selects Mocha. Provisional until prefs land (M5).

use gpui::{Hsla, Rgba, rgb};

/// Official Catppuccin role colors used by panes and chrome.
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
    /// Catppuccin Mocha (default dark flavor).
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

    /// Catppuccin Latte (light flavor).
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

    /// Read `FFT_THEME`. Provisional until prefs land (M5).
    ///
    /// - `FFT_THEME=latte` → Latte
    /// - anything else or unset → Mocha
    pub fn from_env() -> Self {
        Self::select(std::env::var("FFT_THEME").ok().as_deref())
    }

    fn select(theme: Option<&str>) -> Self {
        match theme {
            Some("latte") => Self::latte(),
            _ => Self::mocha(),
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
    fn from_env_selection() {
        assert_eq!(Palette::select(None).base, Palette::mocha().base);
        assert_eq!(Palette::select(Some("mocha")).base, Palette::mocha().base);
        assert_eq!(Palette::select(Some("other")).base, Palette::mocha().base);
        assert_eq!(Palette::select(Some("latte")).base, Palette::latte().base);
        assert_eq!(Palette::select(Some("Latte")).base, Palette::mocha().base);
    }
}
