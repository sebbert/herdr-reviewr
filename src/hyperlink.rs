//! OSC 8 hyperlinks: painted text that leads somewhere carries its URL to the terminal.
//!
//! A link never touches a cell's text, so ratatui's widths, wrapping, and diff stay exactly
//! what they are without links. It travels in three steps:
//!
//! 1. Paint: a widget styles linked text with [`LinkTable::tag`], a reserved underline colour
//!    naming the URL's slot in this frame's table. Nothing in reviewr colours an underline,
//!    and the style rides every span operation (patching, truncation, wrapping, a cursor
//!    fill), so the tag lands on exactly the cells the text does — and a popup's `Clear`
//!    wipes it with the text it covers.
//! 2. Settle ([`settle`]): after the frame is painted, every tagged cell gives up its tag (the
//!    terminal never sees it) and the cells are read back as [`LinkRun`]s.
//! 3. Flush ([`HyperlinkBackend`]): the backend wraps ratatui's crossterm backend and writes
//!    `ESC ] 8 ; ; URL ESC \` … `ESC ] 8 ; ; ESC \` around each run of cells it prints. It
//!    keeps a shadow of what the terminal holds, so a cell whose link changed while its text
//!    did not — invisible to ratatui's diff — is printed again under the right link. Every
//!    draw closes the link it opened, so nothing written later (avatars, the cursor, the
//!    next frame) can inherit it.

use std::io::{self, Write};
use std::sync::Arc;

use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::{Position, Size};
use ratatui::style::{Color, Style};
use unicode_width::UnicodeWidthStr;

/// The longest URL a link carries. Terminals cap the sequence (Ghostty and VTE at a few KB);
/// a longer one paints as plain text rather than as a link the terminal truncates.
const MAX_URL: usize = 2048;

/// One run of painted cells on row `y`, `x0..x1`, leading to `url`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkRun {
    pub y: u16,
    pub x0: u16,
    pub x1: u16,
    pub url: Arc<str>,
}

/// A URL a link may carry: `http(s)` only, with no control or bidi-override character that
/// could end the escape sequence early or disguise it — the same predicate the in-app link
/// opener applies — and within [`MAX_URL`].
#[must_use]
pub fn safe_url(url: &str) -> Option<&str> {
    crate::browser::openable_url(url).ok().filter(|u| u.len() <= MAX_URL)
}

/// This frame's URLs, each in the slot its tag colour names.
#[derive(Debug, Default)]
pub struct LinkTable {
    urls: Vec<Arc<str>>,
}

impl LinkTable {
    /// Forget the last frame's URLs; the renderer calls this before each paint.
    pub fn clear(&mut self) {
        self.urls.clear();
    }

    /// `style` tagged to lead to `url`, or `style` unchanged when there is no URL or it is not
    /// [`safe_url`].
    pub fn tag(&mut self, style: Style, url: Option<&str>) -> Style {
        let Some(url) = url.and_then(safe_url) else { return style };
        let slot = self.urls.iter().position(|u| &**u == url).unwrap_or_else(|| {
            self.urls.push(url.into());
            self.urls.len() - 1
        });
        // 24 bits of slot: far more links than a frame has cells.
        let [_, r, g, b] = u32::try_from(slot).unwrap_or(u32::MAX).to_be_bytes();
        style.underline_color(Color::Rgb(r, g, b))
    }

    /// The URL a cell's underline colour tags, if it is one of this frame's tags.
    fn resolve(&self, color: Color) -> Option<&Arc<str>> {
        let Color::Rgb(r, g, b) = color else { return None };
        self.urls.get(u32::from_be_bytes([0, r, g, b]) as usize)
    }
}

/// Strip every link tag from `buf` and return the runs they marked, row by row. A wide
/// glyph's run covers its hidden trailing cells too, the way the terminal links them.
pub fn settle(buf: &mut Buffer, table: &LinkTable) -> Vec<LinkRun> {
    let area = buf.area;
    let mut runs: Vec<LinkRun> = Vec::new();
    for y in area.top()..area.bottom() {
        let mut x = area.left();
        while x < area.right() {
            let cell = &mut buf[(x, y)];
            let url = table.resolve(cell.underline_color).cloned();
            if url.is_some() {
                cell.underline_color = Color::Reset;
            }
            let span = u16::try_from(cell.symbol().width().max(1)).unwrap_or(1);
            let end = x.saturating_add(span).min(area.right());
            if let Some(url) = url {
                match runs.last_mut() {
                    Some(run) if run.y == y && run.x1 == x && run.url == url => run.x1 = end,
                    _ => runs.push(LinkRun { y, x0: x, x1: end, url }),
                }
            }
            x = end;
        }
    }
    runs
}

/// What the terminal holds, as far as this backend has written it: each cell's content (the
/// cell ratatui last printed there) and the link it was printed under.
#[derive(Default)]
struct Shadow {
    width: usize,
    height: usize,
    cells: Vec<Cell>,
    links: Vec<Option<Arc<str>>>,
}

impl Shadow {
    /// Grow to hold `width × height`, keeping what is known; new cells are blank and
    /// unlinked, which is what ratatui assumes of a cell it never printed.
    fn cover(&mut self, width: usize, height: usize) {
        if width <= self.width && height <= self.height {
            return;
        }
        let (w, h) = (width.max(self.width), height.max(self.height));
        let mut cells = vec![Cell::default(); w * h];
        let mut links = vec![None; w * h];
        for y in 0..self.height {
            for x in 0..self.width {
                let (from, to) = (y * self.width + x, y * w + x);
                cells[to] = std::mem::take(&mut self.cells[from]);
                links[to] = self.links[from].take();
            }
        }
        *self = Self { width: w, height: h, cells, links };
    }

    fn index(&self, x: u16, y: u16) -> usize {
        y as usize * self.width + x as usize
    }

    /// The cells a wide glyph's lead covers: printing there would split the glyph.
    fn hidden(&self) -> Vec<bool> {
        let mut hidden = vec![false; self.cells.len()];
        for y in 0..self.height {
            let mut x = 0;
            while x < self.width {
                let w = self.cells[y * self.width + x].symbol().width().max(1);
                for k in 1..w {
                    if x + k < self.width {
                        hidden[y * self.width + x + k] = true;
                    }
                }
                x += w;
            }
        }
        hidden
    }
}

/// The links the next flush paints. The render closure fills it — `Terminal::draw` flushes
/// to the backend after the closure returns, and the closure cannot reach the backend.
pub type Links = std::rc::Rc<std::cell::RefCell<Vec<LinkRun>>>;

/// ratatui's crossterm backend, with OSC 8 hyperlinks around the runs its [`Links`] name.
/// See the module docs.
pub struct HyperlinkBackend<W: Write> {
    inner: CrosstermBackend<Shared<W>>,
    /// The same writer the inner backend queues into, so the link sequences land in order
    /// between its runs.
    out: Shared<W>,
    links: Links,
    shadow: Shadow,
}

/// One writer shared by the crossterm backend and the link sequences written between its
/// draws.
struct Shared<W>(std::rc::Rc<std::cell::RefCell<W>>);

impl<W> Clone for Shared<W> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<W: Write> Write for Shared<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.borrow_mut().flush()
    }
}

impl<W: Write> std::fmt::Debug for HyperlinkBackend<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HyperlinkBackend")
            .field("links", &self.links.borrow().len())
            .finish_non_exhaustive()
    }
}

impl<W: Write> HyperlinkBackend<W> {
    pub fn new(writer: W) -> Self {
        let out = Shared(std::rc::Rc::new(std::cell::RefCell::new(writer)));
        Self {
            inner: CrosstermBackend::new(out.clone()),
            out,
            links: Links::default(),
            shadow: Shadow::default(),
        }
    }

    /// The handle the render closure sets the frame's links through.
    #[must_use]
    pub fn links(&self) -> Links {
        self.links.clone()
    }

    fn write_link(&mut self, url: Option<&str>) -> io::Result<()> {
        write!(self.out, "\x1b]8;;{}\x1b\\", url.unwrap_or(""))
    }

    /// Forget what the terminal holds: a clear blanked it, unlinked.
    fn forget(&mut self) {
        self.shadow = Shadow::default();
    }
}

impl<W: Write> Backend for HyperlinkBackend<W> {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let diff: Vec<(u16, u16, Cell)> = content.map(|(x, y, c)| (x, y, c.clone())).collect();
        // The shadow covers every cell this draw touches or links.
        let (mut w, mut h) = (self.shadow.width, self.shadow.height);
        for &(x, y, _) in &diff {
            (w, h) = (w.max(x as usize + 1), h.max(y as usize + 1));
        }
        let links = self.links.borrow().clone();
        for run in &links {
            (w, h) = (w.max(run.x1 as usize), h.max(run.y as usize + 1));
        }
        self.shadow.cover(w, h);
        let mut wanted: Vec<Option<Arc<str>>> = vec![None; self.shadow.cells.len()];
        for run in &links {
            for x in run.x0..run.x1 {
                wanted[self.shadow.index(x, run.y)] = Some(run.url.clone());
            }
        }
        let mut emit = vec![false; self.shadow.cells.len()];
        for (x, y, cell) in diff {
            let i = self.shadow.index(x, y);
            self.shadow.cells[i] = cell;
            emit[i] = true;
        }
        // A cell whose link changed under unchanged text: ratatui's diff skips it, so it is
        // printed again here — unless a wide glyph covers it, which printing would split.
        let hidden = self.shadow.hidden();
        for i in 0..emit.len() {
            if !hidden[i] && wanted[i] != self.shadow.links[i] {
                emit[i] = true;
            }
        }
        // Print in row order, one inner draw per run of cells sharing a link, the link opened
        // before the run and closed after the last.
        let width = self.shadow.width;
        let mut open: Option<Arc<str>> = None;
        let mut batch: Vec<(u16, u16, Cell)> = Vec::new();
        for i in (0..emit.len()).filter(|&i| emit[i]) {
            if wanted[i] != open {
                if !batch.is_empty() {
                    self.inner.draw(batch.iter().map(|(x, y, c)| (*x, *y, c)))?;
                    batch.clear();
                }
                self.write_link(wanted[i].as_deref())?;
                open.clone_from(&wanted[i]);
            }
            let (x, y) = ((i % width) as u16, (i / width) as u16);
            batch.push((x, y, self.shadow.cells[i].clone()));
            // A wide glyph links its trailing cells with it.
            let span = self.shadow.cells[i].symbol().width().max(1);
            for k in 0..span.min(width - i % width) {
                self.shadow.links[i + k].clone_from(&wanted[i]);
            }
        }
        if !batch.is_empty() {
            self.inner.draw(batch.iter().map(|(x, y, c)| (*x, *y, c)))?;
        }
        if open.is_some() {
            self.write_link(None)?;
        }
        Ok(())
    }

    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.forget();
        self.inner.append_lines(n)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.forget();
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        // Only a whole-screen clear leaves a known state; the partial ones (an inline
        // viewport's) are not used here, and forgetting stays safe for them too — ratatui
        // repaints what it cleared, and every later print names its own link.
        self.forget();
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> io::Result<Size> {
        self.inner.size()
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> io::Result<()> {
        Backend::flush(&mut self.inner)
    }
}

#[cfg(test)]
mod tests {
    use super::{LinkTable, safe_url, settle};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Style};

    #[test]
    fn only_plain_http_urls_link() {
        assert_eq!(safe_url("https://github.com/ann"), Some("https://github.com/ann"));
        assert_eq!(safe_url("http://x.dev/a?b=c#d"), Some("http://x.dev/a?b=c#d"));
        for bad in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "https://x.dev/\x1b]8;;evil\x1b\\",
            "https://x.dev/\x07",
            "https://x.dev/\u{202E}gpj.exe",
            "",
        ] {
            assert_eq!(safe_url(bad), None, "{bad:?}");
        }
        assert_eq!(safe_url(&format!("https://x.dev/{}", "a".repeat(3000))), None);
    }

    #[test]
    fn tags_settle_into_runs_and_leave_no_trace_in_the_buffer() {
        let mut table = LinkTable::default();
        let a = table.tag(Style::default(), Some("https://a.dev"));
        let b = table.tag(Style::default().fg(Color::Red), Some("https://b.dev"));
        assert_eq!(table.tag(Style::default(), Some("https://a.dev")), a, "one slot per URL");
        assert_eq!(table.tag(Style::default(), Some("ftp://x")), Style::default());
        assert_eq!(table.tag(Style::default(), None), Style::default());

        let mut buf = Buffer::empty(Rect::new(0, 0, 12, 2));
        buf.set_string(0, 0, "ab", a);
        buf.set_string(2, 0, "日c", a);
        buf.set_string(6, 0, "zz", b);
        buf.set_string(0, 1, "x", a);
        let runs = settle(&mut buf, &table);
        let spans: Vec<_> = runs.iter().map(|r| (r.y, r.x0, r.x1, &*r.url)).collect();
        assert_eq!(
            spans,
            [(0, 0, 5, "https://a.dev"), (0, 6, 8, "https://b.dev"), (1, 0, 1, "https://a.dev")],
            "the wide glyph's trailing cell rides its run"
        );
        assert!(buf.content.iter().all(|c| c.underline_color == Color::Reset), "tags stripped");
        assert_eq!(buf[(6, 0)].fg, Color::Red, "the rest of the style stays");
    }
}
