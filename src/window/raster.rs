//! Paints character cells into a 0RGB pixel buffer: glyphs from the system's fonts through a cache,
//! and box, block and braille characters drawn by hand so they join up without gaps between cells.

use std::cell::OnceCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use fontdue::{Font, FontSettings};
use ratatui::buffer::Cell;
use ratatui::style::{Color, Modifier};
use unicode_width::UnicodeWidthStr;

use crate::ui::theme;

#[derive(Clone, Debug)]
pub struct FontSource {
    pub path: PathBuf,
    pub index: u32,
}

/// A font file, parsed only when needed: fontdue prepares every glyph up front, and for a big
/// fallback (Arial Unicode, 50 000 glyphs) that cost half a gigabyte and seconds at start.
///
/// Characters are looked up here, in the Unicode tables only: fontdue merges all of a font's character
/// tables and the old MacRoman one overrides Unicode (Menlo drew `·` as `∑`). Rasterized by glyph index.
struct Face {
    src: FontSource,
    data: OnceCell<Vec<u8>>,
    font: OnceCell<Option<Font>>,
}

impl Face {
    fn new(src: &FontSource) -> Face {
        Face { src: src.clone(), data: OnceCell::new(), font: OnceCell::new() }
    }

    fn data(&self) -> &[u8] {
        self.data.get_or_init(|| std::fs::read(&self.src.path).unwrap_or_default())
    }

    fn font(&self) -> Option<&Font> {
        self.font.get_or_init(|| Font::from_bytes(self.data(), FontSettings { collection_index: self.src.index, ..FontSettings::default() }).ok()).as_ref()
    }

    /// The glyph for `c`, or 0 when this font has none. Doesn't parse the glyphs.
    fn glyph_id(&self, c: char) -> u16 {
        let Ok(face) = ttf_parser::Face::parse(self.data(), self.src.index) else { return 0 };
        let Some(cmap) = face.tables().cmap else { return 0 };
        cmap.subtables.into_iter().filter(|t| t.is_unicode()).find_map(|t| t.glyph_index(c as u32)).map_or(0, |g| g.0)
    }
}

/// One family: regular, bold, italic, bold italic.
pub struct FontSet {
    pub faces: [Option<FontSource>; 4],
    pub fallbacks: Vec<FontSource>,
}

impl FontSet {
    /// `[window] font = "/path/to/font.ttf"` first, then what this system has.
    pub fn system(custom: Option<PathBuf>) -> FontSet {
        let mut set = platform_fonts();
        if let Some(path) = custom {
            set.faces = [Some(FontSource { path, index: 0 }), None, None, None];
        }
        set.fallbacks.splice(0..0, nerd_fonts());
        set
    }
}

/// An installed Nerd Font, for the icons shell prompts use (one file: they all hold the same icons).
fn nerd_fonts() -> Option<FontSource> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let dirs = [home.join("Library/Fonts"), PathBuf::from("/Library/Fonts"), home.join(".local/share/fonts"), PathBuf::from("/usr/share/fonts/TTF")];
    let is_font = |p: &PathBuf| p.extension().is_some_and(|e| e == "ttf" || e == "otf");
    let is_nerd = |p: &PathBuf| p.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase().contains("nerd"));
    let mut found: Vec<PathBuf> = dirs.iter().filter_map(|d| std::fs::read_dir(d).ok()).flatten().flatten().map(|e| e.path()).filter(|p| is_font(p) && is_nerd(p)).collect();
    found.sort();
    found.into_iter().next().map(|path| FontSource { path, index: 0 })
}

#[cfg(target_os = "macos")]
fn platform_fonts() -> FontSet {
    let menlo = |index| Some(FontSource { path: "/System/Library/Fonts/Menlo.ttc".into(), index });
    let fallbacks = ["/System/Library/Fonts/Apple Symbols.ttf", "/System/Library/Fonts/Supplemental/Arial Unicode.ttf"];
    FontSet { faces: [menlo(0), menlo(1), menlo(2), menlo(3)], fallbacks: fallbacks.iter().map(|p| FontSource { path: p.into(), index: 0 }).collect() }
}

#[cfg(not(target_os = "macos"))]
fn platform_fonts() -> FontSet {
    let matched = |pattern: &str| -> Option<FontSource> {
        let out = std::process::Command::new("fc-match").args(["-f", "%{file}\n%{index}", pattern]).output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let (path, index) = text.split_once('\n')?;
        Some(FontSource { path: path.into(), index: index.trim().parse().unwrap_or(0) })
    };
    FontSet {
        faces: [matched("monospace:style=Regular"), matched("monospace:style=Bold"), matched("monospace:style=Italic"), matched("monospace:style=Bold Italic")],
        fallbacks: ["DejaVu Sans", "Noto Sans Symbols2", "Noto Sans CJK JP"].iter().filter_map(|n| matched(n)).collect(),
    }
}

struct Glyph {
    left: i32,
    top: i32,
    w: usize,
    h: usize,
    cover: Vec<u8>,
}

pub struct Canvas {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
    pub cell_w: usize,
    pub cell_h: usize,
    baseline: i32,
    px: f32,
    faces: [Option<Face>; 4],
    fallbacks: Vec<Face>,
    glyphs: HashMap<(char, u8, bool), Rc<Glyph>>,
}

fn rgb(c: Color, default: Color) -> u32 {
    const ANSI: [u32; 16] = [
        0x000000, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5, 0x666666, 0xf14c4c, 0x23d18b, 0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xffffff,
    ];
    match c {
        Color::Reset => rgb(default, Color::Black),
        Color::Rgb(r, g, b) => (r as u32) << 16 | (g as u32) << 8 | b as u32,
        Color::Black => ANSI[0],
        Color::Red => ANSI[1],
        Color::Green => ANSI[2],
        Color::Yellow => ANSI[3],
        Color::Blue => ANSI[4],
        Color::Magenta => ANSI[5],
        Color::Cyan => ANSI[6],
        Color::Gray => ANSI[7],
        Color::DarkGray => ANSI[8],
        Color::LightRed => ANSI[9],
        Color::LightGreen => ANSI[10],
        Color::LightYellow => ANSI[11],
        Color::LightBlue => ANSI[12],
        Color::LightMagenta => ANSI[13],
        Color::LightCyan => ANSI[14],
        Color::White => ANSI[15],
        Color::Indexed(i @ 0..=15) => ANSI[i as usize],
        Color::Indexed(i @ 16..=231) => {
            let i = i as u32 - 16;
            let level = |v: u32| if v == 0 { 0 } else { 55 + v * 40 };
            level(i / 36) << 16 | level(i / 6 % 6) << 8 | level(i % 6)
        }
        Color::Indexed(i) => {
            let v = 8 + (i as u32 - 232) * 10;
            v << 16 | v << 8 | v
        }
    }
}

fn mix(bg: u32, fg: u32, a: u32) -> u32 {
    let ch = |shift: u32| {
        let b = bg >> shift & 255;
        let f = fg >> shift & 255;
        ((b * (255 - a) + f * a + 127) / 255) << shift
    };
    ch(16) | ch(8) | ch(0)
}

impl Canvas {
    /// `px`: font size in device pixels. Fails only when no font at all can be read.
    pub fn new(set: &FontSet, px: f32, width: usize, height: usize) -> anyhow::Result<Canvas> {
        let faces = [0, 1, 2, 3].map(|i| set.faces[i].as_ref().map(Face::new));
        anyhow::ensure!(faces[0].as_ref().and_then(Face::font).is_some(), "no monospace font found");
        let fallbacks = set.fallbacks.iter().map(Face::new).collect();
        let mut c = Canvas { width: 0, height: 0, pixels: vec![], cell_w: 1, cell_h: 1, baseline: 0, px, faces, fallbacks, glyphs: HashMap::new() };
        c.set_px(px);
        c.resize(width, height);
        Ok(c)
    }

    /// New font size: new cell metrics, empty glyph cache. The caller repaints.
    pub fn set_px(&mut self, px: f32) {
        self.px = px;
        self.glyphs.clear();
        let font = self.faces[0].as_ref().and_then(Face::font).expect("checked in new");
        let advance = font.metrics('M', px).advance_width;
        let (ascent, descent) = font.horizontal_line_metrics(px).map(|m| (m.ascent, m.descent)).unwrap_or((px, -px * 0.25));
        self.cell_w = advance.round().max(1.0) as usize;
        self.cell_h = (px * 1.3).ceil().max((ascent - descent).ceil()) as usize;
        self.baseline = ((self.cell_h as f32 - (ascent - descent)) / 2.0 + ascent).round() as i32;
    }

    pub fn resize(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
        self.pixels = vec![0; width * height];
        self.clear();
    }

    pub fn clear(&mut self) {
        self.pixels.fill(rgb(theme::PAPER, Color::Black));
    }

    /// Columns and rows that fit (the rest of the window stays paper).
    pub fn grid(&self) -> (u16, u16) {
        ((self.width / self.cell_w).clamp(1, 4000) as u16, (self.height / self.cell_h).clamp(1, 2000) as u16)
    }

    fn glyph(&mut self, c: char, style: u8, cells: usize) -> Option<Rc<Glyph>> {
        if let Some(g) = self.glyphs.get(&(c, style, cells > 1)) {
            return Some(g.clone());
        }
        let primary = self.faces[style as usize].as_ref().or(self.faces[0].as_ref());
        let candidates = primary.into_iter().chain(self.faces[0].as_ref()).chain(self.fallbacks.iter());
        let (face, id) = candidates.map(|f| (f, f.glyph_id(c))).find(|(_, id)| *id != 0).or_else(|| self.faces[0].as_ref().map(|f| (f, 0)))?;
        // A fallback glyph wider than its cells is scaled down instead of running into the next one.
        let room = (self.cell_w * cells) as f32;
        let font = face.font().or(self.faces[0].as_ref().and_then(Face::font))?;
        let advance = font.metrics_indexed(id, self.px).advance_width;
        let px = if advance > room * 1.02 { self.px * room / advance } else { self.px };
        let (m, cover) = font.rasterize_indexed(id, px);
        let glyph = Rc::new(Glyph { left: m.xmin, top: -(m.ymin + m.height as i32), w: m.width, h: m.height, cover });
        self.glyphs.insert((c, style, cells > 1), glyph.clone());
        Some(glyph)
    }

    fn fill(&mut self, x0: usize, y0: usize, x1: usize, y1: usize, color: u32) {
        let (x1, y1) = (x1.min(self.width), y1.min(self.height));
        for y in y0..y1 {
            if x0 < x1 {
                self.pixels[y * self.width + x0..y * self.width + x1].fill(color);
            }
        }
    }

    pub fn paint_cell(&mut self, col: u16, row: u16, cell: &Cell) {
        let symbol = cell.symbol();
        let cells = UnicodeWidthStr::width(symbol).clamp(1, 2);
        let (x, y) = (col as usize * self.cell_w, row as usize * self.cell_h);
        let (w, h) = (self.cell_w * cells, self.cell_h);
        let m = cell.modifier;
        let (mut fg, mut bg) = (rgb(cell.fg, theme::TEXT), rgb(cell.bg, theme::PAPER));
        if m.contains(Modifier::REVERSED) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if m.contains(Modifier::DIM) {
            fg = mix(bg, fg, 150);
        }
        self.fill(x, y, x + w, y + h, bg);
        if !m.contains(Modifier::HIDDEN) {
            let style = m.contains(Modifier::BOLD) as u8 | (m.contains(Modifier::ITALIC) as u8) << 1;
            for c in symbol.chars().filter(|c| *c != ' ') {
                if !self.draw_special(c, x, y, fg, bg) {
                    self.draw_glyph(c, style, cells, x, y, fg);
                }
            }
        }
        let thick = (self.cell_h / 16).max(1);
        if m.contains(Modifier::UNDERLINED) {
            let color = rgb(cell.underline_color, Color::Reset);
            let color = if cell.underline_color == Color::Reset { fg } else { color };
            self.fill(x, y + h - thick * 2, x + w, y + h - thick, color);
        }
        if m.contains(Modifier::CROSSED_OUT) {
            self.fill(x, y + h / 2, x + w, y + h / 2 + thick, fg);
        }
    }

    fn draw_glyph(&mut self, c: char, style: u8, cells: usize, x: usize, y: usize, fg: u32) {
        let Some(g) = self.glyph(c, style, cells) else { return };
        let (ox, oy) = (x as i32 + g.left, y as i32 + self.baseline + g.top);
        for gy in 0..g.h {
            let py = oy + gy as i32;
            if py < 0 || py >= self.height as i32 {
                continue;
            }
            for gx in 0..g.w {
                let px = ox + gx as i32;
                let a = g.cover[gy * g.w + gx] as u32;
                if a == 0 || px < 0 || px >= self.width as i32 {
                    continue;
                }
                let i = py as usize * self.width + px as usize;
                self.pixels[i] = mix(self.pixels[i], fg, a);
            }
        }
    }

    /// Box, block and braille characters, exact to the cell. False: not one of them.
    fn draw_special(&mut self, c: char, x: usize, y: usize, fg: u32, bg: u32) -> bool {
        let (w, h) = (self.cell_w, self.cell_h);
        if let Some((l, r, u, d)) = box_arms(c) {
            let t = (h / 16).max(1);
            let (cx, cy) = (x + w / 2, y + h / 2);
            let half = |weight: usize| (t * weight).div_ceil(2);
            let span = |weight: usize| t * weight;
            let (hw, vw) = (l.max(r).max(1), u.max(d).max(1));
            if l > 0 {
                self.fill(x, cy - half(l), cx + half(vw), cy - half(l) + span(l), fg);
            }
            if r > 0 {
                self.fill(cx - half(vw), cy - half(r), x + w, cy - half(r) + span(r), fg);
            }
            if u > 0 {
                self.fill(cx - half(u), y, cx - half(u) + span(u), cy + half(hw), fg);
            }
            if d > 0 {
                self.fill(cx - half(d), cy - half(hw), cx - half(d) + span(d), y + h, fg);
            }
            return true;
        }
        if let Some((x0, y0, x1, y1)) = block_eighths(c) {
            self.fill(x + w * x0 / 8, y + h * y0 / 8, x + w * x1 / 8, y + h * y1 / 8, fg);
            return true;
        }
        if let Some(a) = match c {
            '░' => Some(64),
            '▒' => Some(128),
            '▓' => Some(192),
            _ => None,
        } {
            self.fill(x, y, x + w, y + h, mix(bg, fg, a));
            return true;
        }
        if ('\u{2801}'..='\u{28ff}').contains(&c) {
            let bits = c as u32 - 0x2800;
            const DOTS: [(usize, usize); 8] = [(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2), (0, 3), (1, 3)];
            let size = (w / 4).max(1);
            for (i, (dx, dy)) in DOTS.iter().enumerate() {
                if bits >> i & 1 == 1 {
                    let (px, py) = (x + w * (2 * dx + 1) / 4 - size / 2, y + h * (2 * dy + 1) / 8 - size / 2);
                    self.fill(px, py, px + size, py + size, fg);
                }
            }
            return true;
        }
        false
    }
}

/// Arm weights (left, right, up, down) of a box character: 1 light, 2 heavy.
fn box_arms(c: char) -> Option<(usize, usize, usize, usize)> {
    Some(match c {
        '─' => (1, 1, 0, 0),
        '│' => (0, 0, 1, 1),
        '┌' | '╭' => (0, 1, 0, 1),
        '┐' | '╮' => (1, 0, 0, 1),
        '└' | '╰' => (0, 1, 1, 0),
        '┘' | '╯' => (1, 0, 1, 0),
        '├' => (0, 1, 1, 1),
        '┤' => (1, 0, 1, 1),
        '┬' => (1, 1, 0, 1),
        '┴' => (1, 1, 1, 0),
        '┼' => (1, 1, 1, 1),
        '╴' => (1, 0, 0, 0),
        '╵' => (0, 0, 1, 0),
        '╶' => (0, 1, 0, 0),
        '╷' => (0, 0, 0, 1),
        '━' => (2, 2, 0, 0),
        '┃' => (0, 0, 2, 2),
        '┏' => (0, 2, 0, 2),
        '┓' => (2, 0, 0, 2),
        '┗' => (0, 2, 2, 0),
        '┛' => (2, 0, 2, 0),
        '┣' => (0, 2, 2, 2),
        '┫' => (2, 0, 2, 2),
        '┳' => (2, 2, 0, 2),
        '┻' => (2, 2, 2, 0),
        '╋' => (2, 2, 2, 2),
        _ => return None,
    })
}

/// A block character as (x0, y0, x1, y1) in eighths of the cell.
fn block_eighths(c: char) -> Option<(usize, usize, usize, usize)> {
    Some(match c {
        '█' => (0, 0, 8, 8),
        '▀' => (0, 0, 8, 4),
        '▔' => (0, 0, 8, 1),
        '▁' => (0, 7, 8, 8),
        '▂' => (0, 6, 8, 8),
        '▃' => (0, 5, 8, 8),
        '▄' => (0, 4, 8, 8),
        '▅' => (0, 3, 8, 8),
        '▆' => (0, 2, 8, 8),
        '▇' => (0, 1, 8, 8),
        '▉' => (0, 0, 7, 8),
        '▊' => (0, 0, 6, 8),
        '▋' => (0, 0, 5, 8),
        '▌' => (0, 0, 4, 8),
        '▍' => (0, 0, 3, 8),
        '▎' => (0, 0, 2, 8),
        '▏' => (0, 0, 1, 8),
        '▐' => (4, 0, 8, 8),
        '▕' => (7, 0, 8, 8),
        _ => return None,
    })
}

/// Cursor over a copy of the picture: a bar, or an inverted cell in Vim's normal mode.
pub fn draw_cursor(out: &mut [u32], width: usize, cell_w: usize, cell_h: usize, col: u16, row: u16, block: bool) {
    let accent = rgb(theme::ACCENT, Color::Black);
    let (x0, y0) = (col as usize * cell_w, row as usize * cell_h);
    let bar = (cell_w / 6).max(2);
    for y in y0..y0 + cell_h {
        for x in x0..x0 + if block { cell_w } else { bar } {
            if x >= width {
                continue;
            }
            if let Some(p) = out.get_mut(y * width + x) {
                *p = if block { !*p & 0xff_ffff } else { accent };
            }
        }
    }
}

/// Writes 0RGB pixels as a PNG (`--render-png`, `CATLAS_WINDOW_DUMP`).
pub fn write_png(path: &std::path::Path, pixels: &[u32], width: usize, height: usize) -> anyhow::Result<()> {
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(file, width as u32, height as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let bytes: Vec<u8> = pixels.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]).collect();
    enc.write_header()?.write_image_data(&bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas() -> Canvas {
        Canvas::new(&FontSet::system(None), 26.0, 400, 200).expect("this machine has a monospace font")
    }
    fn cell(s: &str, fg: Color, bg: Color) -> Cell {
        let mut c = Cell::default();
        c.set_symbol(s).set_fg(fg).set_bg(bg);
        c
    }
    fn lit(c: &Canvas, col: usize, row: usize, bg: u32) -> usize {
        let (x0, y0) = (col * c.cell_w, row * c.cell_h);
        (y0..y0 + c.cell_h).flat_map(|y| (x0..x0 + c.cell_w).map(move |x| (x, y))).filter(|(x, y)| c.pixels[y * c.width + x] != bg).count()
    }

    #[test]
    fn unicode_wins_over_the_old_mac_table() {
        let c = canvas();
        let id = |ch| c.faces[0].as_ref().unwrap().glyph_id(ch);
        for ch in ['·', 'ü', 'ö', 'ä', 'ß', 'é', '€'] {
            assert_ne!(id(ch), 0, "{ch} is in the font");
        }
        assert_ne!(id('·'), id('∑'), "MacRoman 0xB7 is the sum sign");
        assert_ne!(id('ü'), id('¸'), "MacRoman 0xFC is the cedilla");
        assert_eq!(c.faces[0].as_ref().unwrap().glyph_id('\u{e0000}'), 0, "a character no font has");
    }

    #[test]
    fn fallbacks_load_only_when_a_character_needs_them() {
        let mut c = canvas();
        c.glyph('a', 0, 1);
        assert!(c.fallbacks.iter().all(|f| f.font.get().is_none()), "plain text parses no fallback font");
        assert!(c.faces[1].as_ref().is_none_or(|f| f.font.get().is_none()), "nor bold");
        c.glyph('⎇', 0, 1);
        assert!(c.fallbacks.iter().any(|f| f.font.get().is_some_and(|x| x.is_some())), "a symbol Menlo lacks does");
    }

    #[test]
    fn colours() {
        assert_eq!(rgb(Color::Rgb(1, 2, 3), Color::Black), 0x010203);
        assert_eq!(rgb(Color::Indexed(16), Color::Black), 0);
        assert_eq!(rgb(Color::Indexed(231), Color::Black), 0xffffff);
        assert_eq!(rgb(Color::Indexed(232), Color::Black), 0x080808);
        assert_eq!(rgb(Color::Reset, theme::TEXT), rgb(theme::TEXT, Color::Black));
        assert_eq!(mix(0, 0xffffff, 255), 0xffffff);
        assert_eq!(mix(0x102030, 0xffffff, 0), 0x102030);
    }

    #[test]
    fn text_marks_its_cell_and_no_other() {
        let mut c = canvas();
        let (white, black) = (Color::Rgb(255, 255, 255), Color::Rgb(0, 0, 0));
        c.paint_cell(0, 0, &cell(" ", white, black));
        c.paint_cell(1, 0, &cell("M", white, black));
        c.paint_cell(2, 0, &cell(" ", white, black));
        assert_eq!(lit(&c, 0, 0, 0), 0);
        assert!(lit(&c, 1, 0, 0) > 20, "a glyph was drawn");
        assert_eq!(lit(&c, 2, 0, 0), 0);
    }

    #[test]
    fn box_lines_reach_the_cell_edges() {
        let mut c = canvas();
        let (white, black) = (Color::Rgb(255, 255, 255), Color::Rgb(0, 0, 0));
        c.paint_cell(0, 0, &cell("┼", white, black));
        let (w, h) = (c.cell_w, c.cell_h);
        let row_mid = h / 2;
        assert_eq!(c.pixels[row_mid * c.width], 0xffffff, "left edge");
        assert_eq!(c.pixels[row_mid * c.width + w - 1], 0xffffff, "right edge");
        assert_eq!(c.pixels[w / 2], 0xffffff, "top edge");
        assert_eq!(c.pixels[(h - 1) * c.width + w / 2], 0xffffff, "bottom edge");
        c.paint_cell(1, 0, &cell("█", white, black));
        assert_eq!(lit(&c, 1, 0, 0), w * h, "a full block fills the cell");
        c.paint_cell(2, 0, &cell("⠋", white, black));
        assert!(lit(&c, 2, 0, 0) > 0, "braille is drawn without a font");
    }

    #[test]
    fn styles_change_pixels() {
        let mut c = canvas();
        let mut under = cell(" ", Color::Rgb(255, 255, 255), Color::Rgb(0, 0, 0));
        under.set_style(ratatui::style::Style::new().underline_color(Color::Rgb(255, 0, 0)).add_modifier(Modifier::UNDERLINED));
        c.paint_cell(0, 0, &under);
        assert!((0..c.cell_h).any(|y| c.pixels[y * c.width] == 0xff0000), "the underline has its own colour");
        let rev = cell(" ", Color::Rgb(255, 255, 255), Color::Rgb(0, 0, 0)).set_style(ratatui::style::Style::new().add_modifier(Modifier::REVERSED)).clone();
        c.paint_cell(1, 0, &rev);
        assert_eq!(c.pixels[c.cell_w], 0xffffff, "reversed swaps the colours");
    }

    #[test]
    fn cursor_shapes() {
        let mut px = vec![0u32; 100 * 100];
        draw_cursor(&mut px, 100, 10, 20, 1, 1, true);
        assert_eq!(px[25 * 100 + 15], 0xffffff);
        assert_eq!(px[25 * 100 + 25], 0);
        let mut px = vec![0u32; 100 * 100];
        draw_cursor(&mut px, 100, 10, 20, 9, 4, false);
        assert_eq!(px[85 * 100 + 90], rgb(theme::ACCENT, Color::Black));
    }
}
