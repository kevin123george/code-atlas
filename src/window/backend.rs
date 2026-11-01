//! A ratatui backend that paints into a `Canvas` instead of writing escape codes. `ui::draw` does
//! not know the difference, so the whole interface runs unchanged in a window (decision 0017).

use std::io;

use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};

use super::raster::Canvas;

pub struct GridBackend {
    pub canvas: Canvas,
    pub cursor: Position,
    pub cursor_on: bool,
    /// Something changed since the last present.
    pub dirty: bool,
}

impl GridBackend {
    pub fn new(canvas: Canvas) -> Self {
        GridBackend { canvas, cursor: Position::ORIGIN, cursor_on: false, dirty: true }
    }
}

impl Backend for GridBackend {
    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        for (x, y, cell) in content {
            self.canvas.paint_cell(x, y, cell);
            self.dirty = true;
        }
        Ok(())
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.dirty |= self.cursor_on;
        self.cursor_on = false;
        Ok(())
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.dirty |= !self.cursor_on;
        self.cursor_on = true;
        Ok(())
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        Ok(self.cursor)
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        let p = position.into();
        self.dirty |= p != self.cursor;
        self.cursor = p;
        Ok(())
    }

    fn clear(&mut self) -> io::Result<()> {
        self.canvas.clear();
        self.dirty = true;
        Ok(())
    }

    fn clear_region(&mut self, _clear_type: ClearType) -> io::Result<()> {
        self.clear()
    }

    fn size(&self) -> io::Result<Size> {
        let (cols, rows) = self.canvas.grid();
        Ok(Size::new(cols, rows))
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        let (cols, rows) = self.canvas.grid();
        Ok(WindowSize { columns_rows: Size::new(cols, rows), pixels: Size::new(self.canvas.width.min(65535) as u16, self.canvas.height.min(65535) as u16) })
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
