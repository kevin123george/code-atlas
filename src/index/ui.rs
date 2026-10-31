//! The status-bar indicator: `⟳ Indexing 45%` while the index is busy, nothing when it's idle.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::app::App;
use crate::ui::theme::*;

/// Draws the indicator left of the status bar's right part (`right_w` columns wide); returns
/// the columns it took.
pub fn draw_status(buf: &mut Buffer, app: &App, area: Rect, right_w: u16) -> u16 {
    let Some(text) = app.project.code_index.as_ref().and_then(|i| i.busy()) else { return 0 };
    let label = format!(" ⟳ {text} ·");
    let w = label.chars().count() as u16;
    if right_w + w + 20 > area.width {
        return 0;
    }
    buf.set_string(area.right() - right_w - w, area.y, &label, Style::new().fg(ACCENT).bg(SURFACE_2));
    w
}
