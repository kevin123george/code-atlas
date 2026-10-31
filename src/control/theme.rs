//! Themes (decision 0016): a registry of named palettes. Ledger Terminal dark is the default
//! (what every module draws with); Ledger Terminal light and High contrast ship with it; more
//! (track E's tokyonight / catppuccin, decision 0017) are added as [`Palette`]s to [`PALETTES`].
//!
//! How it works: everything in catlas draws with the dark tokens of `ui::theme` (plus a few local
//! colours in `git::ui`, `git::view_ui`). Instead of threading a theme through every draw call,
//! [`apply`] recolours the finished frame at the end of `ui::draw`: each dark token has an exact
//! counterpart in the palette ([`TOKENS`] lists them; a test checks every palette covers all),
//! and any other colour (programs in the terminal pane) gets the palette's [`Fallback`]. Syntax
//! captures can be coloured one by one too: `Palette::syntax` overrides a [`Token`]'s colour
//! (`ui::theme::token_style` asks [`syntax_fg`]; the cell carries a private marker colour that
//! [`apply`] swaps for the override). Parallel features keep using `theme::PAPER` and friends and
//! get every theme for free.

use std::sync::atomic::{AtomicUsize, Ordering};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use crate::syntax::Token;
use crate::ui::theme as t;

/// What happens to colours a palette doesn't list (terminal programs' own colours).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fallback {
    Keep,
    /// Light text on dark becomes dark text on light (lightness flipped, hue kept).
    FlipLightness,
    /// Pushed away from mid grey.
    MoreContrast,
}

pub struct Palette {
    /// The value in `settings.toml` (`theme = "ledger-light"`).
    pub key: &'static str,
    pub name: &'static str,
    /// (dark token, this palette's colour) for every entry of [`TOKENS`].
    pub colors: &'static [(Color, Color)],
    /// Per-capture syntax colours (beyond what the token mapping gives).
    pub syntax: &'static [(Token, Color)],
    pub fallback: Fallback,
}

const fn rgb(v: u32) -> Color {
    Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// Every colour catlas draws with (the dark palette). A palette maps each of these.
#[cfg_attr(not(test), allow(dead_code))]
pub const TOKENS: &[Color] = &[
    t::PAPER,
    t::SURFACE_1,
    t::SURFACE_2,
    t::TEXT,
    t::TEXT_2,
    t::MUTED,
    t::GRID,
    t::BASELINE,
    t::GHOST,
    t::ACCENT,
    t::GOOD,
    t::CRITICAL,
    t::WARNING,
    t::SELECTION,
    t::S_BLUE,
    t::S_ORANGE,
    t::S_AQUA,
    t::S_YELLOW,
    t::S_MAGENTA,
    t::S_VIOLET,
    t::S_TEAL,
    crate::git::ui::ADD_BG,
    crate::git::ui::DEL_BG,
    crate::git::view_ui::ADD_HI,
    crate::git::view_ui::DEL_HI,
];

pub const LEDGER_DARK: Palette = Palette { key: "ledger-dark", name: "Ledger Terminal dark", colors: &[], syntax: &[], fallback: Fallback::Keep };

/// The Cashflow Atlas light tokens (finance-tracker `index.html`), a few darkened for contrast
/// on paper (orange, aqua, yellow, magenta, muted).
pub const LEDGER_LIGHT: Palette = Palette {
    key: "ledger-light",
    name: "Ledger Terminal light",
    colors: &[
        (t::PAPER, rgb(0xf6f7f9)),
        (t::SURFACE_1, rgb(0xffffff)),
        (t::SURFACE_2, rgb(0xe6eaf1)),
        (t::TEXT, rgb(0x0b0e14)),
        (t::TEXT_2, rgb(0x4b5566)),
        (t::MUTED, rgb(0x7d8697)),
        (t::GRID, rgb(0xe3e7ee)),
        (t::BASELINE, rgb(0xc7cddb)),
        (t::GHOST, rgb(0xcfd4de)),
        (t::ACCENT, rgb(0x1d5fb8)),
        (t::GOOD, rgb(0x0a8f5b)),
        (t::CRITICAL, rgb(0xd1453b)),
        (t::WARNING, rgb(0xb77a00)),
        (t::SELECTION, rgb(0xd2dff1)),
        (t::S_BLUE, rgb(0x2a78d6)),
        (t::S_ORANGE, rgb(0xd9541f)),
        (t::S_AQUA, rgb(0x14935f)),
        (t::S_YELLOW, rgb(0xa87200)),
        (t::S_MAGENTA, rgb(0xd04c88)),
        (t::S_VIOLET, rgb(0x4a3aa7)),
        (t::S_TEAL, rgb(0x0a6e8a)),
        (crate::git::ui::ADD_BG, rgb(0xdff3ea)),
        (crate::git::ui::DEL_BG, rgb(0xfbe3e1)),
        (crate::git::view_ui::ADD_HI, rgb(0xb5e3cd)),
        (crate::git::view_ui::DEL_HI, rgb(0xf4b9b4)),
    ],
    syntax: &[],
    fallback: Fallback::FlipLightness,
};

/// Black paper, white text, bright accents (WCAG AAA for text; tests check it).
pub const HIGH_CONTRAST: Palette = Palette {
    key: "high-contrast",
    name: "High contrast",
    colors: &[
        (t::PAPER, rgb(0x000000)),
        (t::SURFACE_1, rgb(0x121212)),
        (t::SURFACE_2, rgb(0x1f1f1f)),
        (t::TEXT, rgb(0xffffff)),
        (t::TEXT_2, rgb(0xe6e6e6)),
        (t::MUTED, rgb(0xb8b8b8)),
        (t::GRID, rgb(0x5c5c5c)),
        (t::BASELINE, rgb(0x8c8c8c)),
        (t::GHOST, rgb(0x6b6b6b)),
        (t::ACCENT, rgb(0x00f0ff)),
        (t::GOOD, rgb(0x3dff9e)),
        (t::CRITICAL, rgb(0xff6b6b)),
        (t::WARNING, rgb(0xffd23f)),
        (t::SELECTION, rgb(0x00476e)),
        (t::S_BLUE, rgb(0x7fb2ff)),
        (t::S_ORANGE, rgb(0xffab70)),
        (t::S_AQUA, rgb(0x4dffb8)),
        (t::S_YELLOW, rgb(0xffe066)),
        (t::S_MAGENTA, rgb(0xff9cc8)),
        (t::S_VIOLET, rgb(0xb8a8ff)),
        (t::S_TEAL, rgb(0x5ee0ff)),
        (crate::git::ui::ADD_BG, rgb(0x003a20)),
        (crate::git::ui::DEL_BG, rgb(0x4a0006)),
        (crate::git::view_ui::ADD_HI, rgb(0x00703a)),
        (crate::git::view_ui::DEL_HI, rgb(0x8a0010)),
    ],
    syntax: &[(Token::Comment, rgb(0xc8c8c8))],
    fallback: Fallback::MoreContrast,
};

/// The registry. HOOK(track E, decision 0017): tokyonight, catppuccin go here.
pub static PALETTES: &[&Palette] = &[&LEDGER_DARK, &LEDGER_LIGHT, &HIGH_CONTRAST];

/// A theme = an index into [`PALETTES`] (0 = Ledger Terminal dark).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Theme(pub usize);

impl Theme {
    pub fn all() -> impl Iterator<Item = Theme> {
        (0..PALETTES.len()).map(Theme)
    }

    pub fn palette(self) -> &'static Palette {
        PALETTES.get(self.0).copied().unwrap_or(&LEDGER_DARK)
    }

    pub fn key(self) -> &'static str {
        self.palette().key
    }

    pub fn name(self) -> &'static str {
        self.palette().name
    }

    pub fn from_key(s: &str) -> Option<Theme> {
        PALETTES.iter().position(|p| p.key == s.trim()).map(Theme)
    }

    pub fn next(self) -> Theme {
        Theme((self.0 + 1) % PALETTES.len())
    }

    pub fn is_default(self) -> bool {
        self.0 == 0
    }
}

/// The theme in use (read by `ui::theme::token_style` for per-capture colours).
static ACTIVE: AtomicUsize = AtomicUsize::new(0);

pub fn set_active(th: Theme) {
    ACTIVE.store(th.0, Ordering::Relaxed);
}

pub fn active() -> Theme {
    Theme(ACTIVE.load(Ordering::Relaxed))
}

/// Marker colour for a syntax override: `Rgb(1, 2, n)`, swapped by [`apply`].
fn marker(tok: Token) -> Color {
    Color::Rgb(1, 2, tok as u8)
}

/// The colour `token_style` should draw `tok` with in the active theme, when the palette
/// overrides it (a marker that [`apply`] replaces).
pub fn syntax_fg(tok: Token) -> Option<Color> {
    syntax_fg_in(active().palette(), tok)
}

pub fn syntax_fg_in(p: &Palette, tok: Token) -> Option<Color> {
    p.syntax.iter().any(|(k, _)| *k == tok).then(|| marker(tok))
}

/// What `c` (a dark-theme colour) looks like in `p`.
pub fn map(p: &Palette, c: Color) -> Color {
    if p.colors.is_empty() && p.syntax.is_empty() && p.fallback == Fallback::Keep {
        return c;
    }
    let Color::Rgb(r, g, b) = c else { return c };
    if (r, g) == (1, 2) {
        if let Some((_, col)) = p.syntax.iter().find(|(k, _)| *k as u8 == b) {
            return *col;
        }
    }
    if let Some(&(_, out)) = p.colors.iter().find(|(d, _)| *d == c) {
        return out;
    }
    let (h, s, l) = to_hsl(r, g, b);
    let l = match p.fallback {
        Fallback::Keep => return c,
        Fallback::FlipLightness => (1.0 - l).clamp(0.10, 0.97),
        Fallback::MoreContrast => (0.5 + (l - 0.5) * 1.5).clamp(0.0, 1.0),
    };
    let (r, g, b) = from_hsl(h, s, l);
    Color::Rgb(r, g, b)
}

/// Recolours every cell of `area` for `theme` (no-op for the dark default).
pub fn apply(buf: &mut Buffer, area: Rect, theme: Theme) {
    if theme.is_default() {
        return;
    }
    let p = theme.palette();
    let area = area.intersection(buf.area);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let c = &mut buf[(x, y)];
            c.fg = map(p, c.fg);
            c.bg = map(p, c.bg);
            c.underline_color = map(p, c.underline_color);
        }
    }
}

fn to_hsl(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if max == min {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h / 6.0, s, l)
}

fn from_hsl(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    if s == 0.0 {
        let v = (l * 255.0).round() as u8;
        return (v, v, v);
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let hue = |mut t: f32| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    let c = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    (c(hue(h + 1.0 / 3.0)), c(hue(h)), c(hue(h - 1.0 / 3.0)))
}

#[cfg(test)]
pub fn luminance(c: Color) -> f32 {
    let Color::Rgb(r, g, b) = c else { return 0.0 };
    let ch = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * ch(r) + 0.7152 * ch(g) + 0.0722 * ch(b)
}

#[cfg(test)]
pub fn contrast(a: Color, b: Color) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip_and_cycle() {
        for th in Theme::all() {
            assert_eq!(Theme::from_key(th.key()), Some(th));
        }
        assert_eq!(Theme::from_key("neon"), None);
        assert_eq!(Theme::default().key(), "ledger-dark");
        assert_eq!(Theme(0).next(), Theme(1));
        assert_eq!(Theme(PALETTES.len() - 1).next(), Theme(0));
        let keys: Vec<&str> = PALETTES.iter().map(|p| p.key).collect();
        for (i, k) in keys.iter().enumerate() {
            assert!(!keys[..i].contains(k), "{k} registered twice");
        }
    }

    #[test]
    fn every_palette_covers_every_token_once() {
        for (i, d) in TOKENS.iter().enumerate() {
            assert!(!TOKENS[..i].contains(d), "{d:?} twice in TOKENS");
        }
        for p in PALETTES.iter().skip(1) {
            for tok in TOKENS {
                let n = p.colors.iter().filter(|(d, _)| d == tok).count();
                assert_eq!(n, 1, "{}: {tok:?} mapped {n} times", p.key);
            }
            assert_eq!(p.colors.len(), TOKENS.len(), "{}: extra entries", p.key);
        }
        assert_eq!(map(&LEDGER_LIGHT, t::PAPER), rgb(0xf6f7f9));
        assert_eq!(map(&HIGH_CONTRAST, t::TEXT), rgb(0xffffff));
        assert_eq!(map(&LEDGER_DARK, t::ACCENT), t::ACCENT);
        assert_eq!(map(&LEDGER_LIGHT, Color::Reset), Color::Reset, "named colours stay");
    }

    #[test]
    fn text_reads_on_its_backgrounds() {
        for th in Theme::all() {
            let p = th.palette();
            for bg in [t::PAPER, t::SURFACE_1, t::SURFACE_2, t::SELECTION] {
                let bg = map(p, bg);
                assert!(contrast(map(p, t::TEXT), bg) >= 7.0, "{} text on {bg:?}", p.key);
                assert!(contrast(map(p, t::TEXT_2), bg) >= 4.5, "{} secondary text on {bg:?}", p.key);
                assert!(contrast(map(p, t::ACCENT), bg) >= 3.0, "{} accent on {bg:?}", p.key);
            }
            assert!(contrast(map(p, t::MUTED), map(p, t::PAPER)) >= 3.0, "{} muted on paper", p.key);
            let min = if p.key == "high-contrast" { 7.0 } else { 3.0 };
            for tok in [t::S_BLUE, t::S_ORANGE, t::S_AQUA, t::S_YELLOW, t::S_MAGENTA, t::S_VIOLET, t::S_TEAL, t::GOOD, t::CRITICAL, t::WARNING] {
                assert!(contrast(map(p, tok), map(p, t::PAPER)) >= min, "{}: {tok:?} on paper", p.key);
            }
        }
    }

    #[test]
    fn unknown_colours_get_the_fallback() {
        let white = rgb(0xc8cede); // terminal ANSI white
        assert!(luminance(map(&LEDGER_LIGHT, white)) < 0.2);
        assert!(luminance(map(&HIGH_CONTRAST, white)) > luminance(white));
        assert_eq!(from_hsl(0.0, 1.0, 0.5), (255, 0, 0));
        let (h, s, l) = to_hsl(0x2c, 0xe0, 0xf5);
        assert_eq!(from_hsl(h, s, l), (0x2c, 0xe0, 0xf5));
    }

    #[test]
    fn syntax_overrides_go_through_a_marker() {
        let m = syntax_fg_in(&HIGH_CONTRAST, Token::Comment).expect("high contrast overrides comments");
        assert_eq!(map(&HIGH_CONTRAST, m), rgb(0xc8c8c8));
        assert_eq!(syntax_fg_in(&HIGH_CONTRAST, Token::Keyword), None);
        assert_eq!(syntax_fg_in(&LEDGER_DARK, Token::Comment), None);
    }

    #[test]
    fn apply_recolours_a_buffer() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 2));
        buf.set_style(buf.area, ratatui::style::Style::new().fg(t::TEXT).bg(t::PAPER));
        let light = Theme::from_key("ledger-light").unwrap();
        apply(&mut buf, Rect::new(0, 0, 4, 2), light);
        assert_eq!(buf[(3, 1)].bg, rgb(0xf6f7f9));
        assert_eq!(buf[(0, 0)].fg, rgb(0x0b0e14));
        let before = buf.clone();
        apply(&mut buf, Rect::new(0, 0, 4, 2), Theme::default());
        assert_eq!(buf, before);
    }
}
