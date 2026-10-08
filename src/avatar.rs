//! Author avatars in the PR conversation: the `●` at each turn's byline painted as the
//! author's picture, through the Kitty graphics protocol's Unicode placeholders.
//!
//! The pieces stay apart so none of them can hold a frame. The PR fetch carries avatar URLs
//! as strings and nothing more. The [`Fetcher`] downloads and decodes on its own threads
//! (`curl`, bounded concurrency, tight timeouts). The [`Store`] holds what landed, keyed by
//! URL for the session. The event loop transmits a landed image once, out of band, and the
//! renderer paints ordinary text cells — U+10EEEE plus row/column diacritics, the image id
//! in the foreground colour — that the terminal swaps for the picture. Anything missing, a
//! terminal that never answered the [`PROBE`], a failed download, keeps the dot. The
//! protocol pieces, the probe, and the fetcher are `crate::graphics`, shared with inline
//! PR images; what is here is the avatar's own: its circle, its cells, its store.

use std::collections::HashMap;

use image::RgbaImage;

#[cfg(test)]
use crate::graphics::base64;
pub use crate::graphics::{
    FALLBACK_CELL, Fed, PLACEHOLDER, PROBE, PROBE_WINDOW, ProbeFilter, delete, id_rgb, parse_reply,
};

/// The download workers: enough to fill a screen of cards quickly, few enough that a slow
/// avatar host never fans out a crowd of `curl`s.
const WORKERS: usize = 4;

/// The source image's longest side once decoded — far above any cell, far below a photo.
const SOURCE_MAX: u32 = 128;

/// The text of placeholder cell `col` (row 0) of a placement.
#[must_use]
pub fn placeholder_cell(col: usize) -> String {
    crate::graphics::placeholder(0, col.min(2))
}

/// The escape sequences that transmit `img` as image `id` with a virtual placement `cols`
/// cells wide and one row tall, replies suppressed, the RGBA payload chunked the way the
/// protocol requires.
#[must_use]
pub fn transmit(id: u32, cols: u8, img: &RgbaImage) -> Vec<u8> {
    let head =
        format!("a=T,U=1,f=32,s={},v={},c={cols},r=1,i={id},q=2,", img.width(), img.height());
    crate::graphics::chunked(&head, &crate::graphics::base64(img.as_raw()))
}

/// What sizes the avatar's circle: the row's height, or the avatar cells' width.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Fit {
    #[default]
    Height,
    Width,
}

impl Fit {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Height => "height",
            Self::Width => "width",
        }
    }
}

/// Where one avatar paints around its byline's dot, in cells and canvas pixels.
///
/// The box timeline is `│`, a blank, the dot, a blank, then text: the dot's cell (and with
/// width 2 the blank after it) are the avatar's own cells, and the blank cells beside them
/// may take an overhang. The border and the text never do — the circle clips there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    /// Blank cells the placement covers before the dot's cell: 0 or 1.
    pub before: u8,
    /// The placement's width in cells, `before` included.
    pub cols: u8,
    /// The canvas the image is drawn on: exactly `cols` cells by one row, so the terminal
    /// fits it to the placement without stretching.
    pub canvas: (u32, u32),
    /// The circle's diameter in pixels.
    pub diameter: u32,
    /// The circle's centre on the canvas, in pixels.
    pub centre: (f32, f32),
}

/// The geometry for `width` avatar cells (1 or 2) at `cell` pixels under `fit`.
///
/// `Width`: the diameter is the avatar cells' total width (capped at the row's height, so
/// the circle never clips top and bottom), centred on them, transparent above and below.
/// `Height`: the diameter is the row's height, centred on the avatar cells, overhanging into
/// the blank cells beside them — the one after the border, and at width 1 the one after the
/// dot — and clipped at the cell edge where a border or text cell begins.
#[must_use]
// Pixel counts here are a cell's size or a few of them, far inside f32's exact range.
#[allow(clippy::cast_precision_loss)]
pub fn geometry(cell: (u16, u16), width: u8, fit: Fit) -> Geometry {
    let (cw, ch) = (u32::from(cell.0.max(1)), u32::from(cell.1.max(1)));
    let width = width.clamp(1, 2);
    let span = cw * u32::from(width);
    let mid = span as f32 / 2.0;
    match fit {
        Fit::Width => {
            let diameter = span.min(ch);
            Geometry {
                before: 0,
                cols: width,
                canvas: (span, ch),
                diameter,
                centre: (mid, ch as f32 / 2.0),
            }
        }
        Fit::Height => {
            let diameter = ch;
            let overhang = diameter as f32 / 2.0 - mid;
            let need = if overhang > 0.0 { (overhang / cw as f32).ceil() as u8 } else { 0 };
            // One blank before the dot; one after it only when the dot's cell is the avatar's
            // last (width 1) — at width 2 the next cell is text.
            let before = need.min(1);
            let after = if width == 1 { need.min(1) } else { 0 };
            let cols = before + width + after;
            Geometry {
                before,
                cols,
                canvas: (cw * u32::from(cols), ch),
                diameter,
                centre: (cw as f32 * f32::from(before) + mid, ch as f32 / 2.0),
            }
        }
    }
}

/// The avatar as painted: `src` scaled into the geometry's circle, anti-aliased against
/// transparency, everything outside the circle — and outside the canvas — left clear.
#[must_use]
// Pixel coordinates within a cell-sized canvas, far inside f32's exact range.
#[allow(clippy::cast_precision_loss)]
pub fn paint(src: &RgbaImage, g: &Geometry) -> RgbaImage {
    let d = g.diameter.max(1);
    let scaled = image::imageops::resize(src, d, d, image::imageops::FilterType::Triangle);
    let mut out = RgbaImage::new(g.canvas.0, g.canvas.1);
    let r = d as f32 / 2.0;
    let (ox, oy) = ((g.centre.0 - r).round() as i64, (g.centre.1 - r).round() as i64);
    for (x, y, px) in scaled.enumerate_pixels() {
        let (cx, cy) = (ox + i64::from(x), oy + i64::from(y));
        if cx < 0 || cy < 0 || cx >= i64::from(g.canvas.0) || cy >= i64::from(g.canvas.1) {
            continue;
        }
        let (dx, dy) = (x as f32 + 0.5 - r, y as f32 + 0.5 - r);
        let coverage = (r - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
        let mut px = *px;
        px.0[3] = (f32::from(px.0[3]) * coverage).round() as u8;
        out.put_pixel(cx as u32, cy as u32, px);
    }
    out
}

/// Decode a downloaded avatar into a small RGBA source, `None` for anything undecodable.
#[must_use]
pub fn decode(bytes: &[u8]) -> Option<RgbaImage> {
    let img = image::load_from_memory(bytes).ok()?;
    let img = if img.width().max(img.height()) > SOURCE_MAX {
        img.thumbnail(SOURCE_MAX, SOURCE_MAX)
    } else {
        img
    };
    Some(img.to_rgba8())
}

/// Download one avatar with `curl`: fail on HTTP errors, follow redirects, a hard time
/// limit, a size cap, and nothing but `http(s)`. `None` on any failure — the dot stays.
#[must_use]
pub fn curl(url: &str) -> Option<Vec<u8>> {
    let limits =
        crate::graphics::Limits { max_bytes: 2_000_000, max_time: 8, https_redirects: false };
    crate::graphics::curl(url, limits, None)
}

/// One finished download: the URL and its decoded source, `None` when it failed.
pub type Landing = crate::graphics::Landing<RgbaImage>;

/// The avatar worker: the shared [`crate::graphics::Fetcher`] downloading and decoding
/// avatars, never with a token.
pub type Fetcher = crate::graphics::Fetcher<RgbaImage>;

impl crate::graphics::Fetcher<RgbaImage> {
    /// Spawn the avatar workers over `download` (the real one is [`curl`]).
    pub fn spawn(download: fn(&str) -> Option<Vec<u8>>) -> Self {
        Self::spawn_with(WORKERS, move |r: &crate::graphics::Request| {
            download(&r.url).as_deref().and_then(decode)
        })
    }
}

/// One URL's session state.
#[derive(Debug)]
enum Entry {
    Pending,
    Failed,
    Ready {
        id: u32,
        source: RgbaImage,
        /// The geometry it was last transmitted at; `None` until transmitted (again, after
        /// anything that could have dropped it).
        sent: Option<Geometry>,
    },
}

/// Every avatar this session has asked for, keyed by URL. A failed URL stays failed — a dot —
/// for the session, so a dead host is asked once.
#[derive(Debug, Default)]
pub struct Store {
    entries: HashMap<String, Entry>,
    next_id: u32,
}

/// The first image id reviewr uses — clear of the probe's `31`.
const FIRST_ID: u32 = 0x00_52_00;

impl Store {
    /// The image id to paint for `url`: only once it is on the terminal.
    #[must_use]
    pub fn painted_id(&self, url: &str) -> Option<u32> {
        match self.entries.get(url) {
            Some(Entry::Ready { id, sent: Some(_), .. }) => Some(*id),
            _ => None,
        }
    }

    /// Whether `url` has never been asked for.
    #[must_use]
    pub fn is_new(&self, url: &str) -> bool {
        !self.entries.contains_key(url)
    }

    pub fn mark_requested(&mut self, url: &str) {
        self.entries.entry(url.to_string()).or_insert(Entry::Pending);
    }

    /// Whether any download is still out.
    #[must_use]
    pub fn pending(&self) -> bool {
        self.entries.values().any(|e| matches!(e, Entry::Pending))
    }

    /// Land one download: a decoded source becomes ready (an id, not yet transmitted), a
    /// failure stays a dot.
    pub fn land(&mut self, url: String, image: Option<RgbaImage>) {
        let entry = match image {
            Some(source) => {
                let id = FIRST_ID + self.next_id;
                self.next_id += 1;
                Entry::Ready { id, source, sent: None }
            }
            None => Entry::Failed,
        };
        self.entries.insert(url, entry);
    }

    /// The transmissions owed at this geometry: every ready image not yet on the terminal
    /// in this shape. Marks them sent.
    pub fn transmissions(&mut self, g: Geometry) -> Vec<u8> {
        let mut out = Vec::new();
        for entry in self.entries.values_mut() {
            if let Entry::Ready { id, source, sent } = entry
                && *sent != Some(g)
            {
                out.extend(transmit(*id, g.cols, &paint(source, &g)));
                *sent = Some(g);
            }
        }
        out
    }

    /// The deletions for every image on the terminal, which then counts as unsent — the
    /// next transmission pass puts them back.
    pub fn deletions(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        for entry in self.entries.values_mut() {
            if let Entry::Ready { id, sent, .. } = entry
                && sent.take().is_some()
            {
                out.extend(delete(*id));
            }
        }
        out
    }

    /// Forget what the terminal holds, after anything that could have dropped it (a resize):
    /// the next pass re-transmits. Painted cells fall back to dots until then.
    pub fn forget_sent(&mut self) {
        for entry in self.entries.values_mut() {
            if let Entry::Ready { sent, .. } = entry {
                *sent = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn a_placeholder_cell_is_one_column_wide_with_its_row_and_column_marks() {
        assert_eq!(placeholder_cell(0), "\u{10EEEE}\u{0305}\u{0305}");
        assert_eq!(placeholder_cell(1), "\u{10EEEE}\u{0305}\u{030D}");
        for col in 0..2 {
            assert_eq!(placeholder_cell(col).width(), 1, "one cell, like the dot");
        }
        assert_eq!(id_rgb(0x12_34_56), (0x12, 0x34, 0x56));
        assert_eq!(id_rgb(FIRST_ID), (0, 0x52, 0));
    }

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0, 0, 0]), "AAAA");
    }

    #[test]
    fn a_transmission_creates_a_quiet_virtual_placement_and_chunks_its_payload() {
        let small = RgbaImage::new(2, 1);
        let seq = String::from_utf8(transmit(7, 2, &small)).unwrap();
        assert_eq!(seq, "\x1b_Ga=T,U=1,f=32,s=2,v=1,c=2,r=1,i=7,q=2,m=0;AAAAAAAAAAA=\x1b\\");

        // 64×64 RGBA is 16 KiB raw, about 22 KiB of base64: several chunks, the control keys
        // on the first only, every chunk but the last flagged `m=1`.
        let big = RgbaImage::new(64, 64);
        let seq = String::from_utf8(transmit(9, 1, &big)).unwrap();
        let chunks: Vec<&str> = seq.split("\x1b\\").filter(|c| !c.is_empty()).collect();
        assert!(chunks.len() > 1);
        assert!(chunks[0].starts_with("\x1b_Ga=T,U=1,f=32,s=64,v=64,c=1,r=1,i=9,q=2,m=1;"));
        assert!(chunks[1..chunks.len() - 1].iter().all(|c| c.starts_with("\x1b_Gm=1;")));
        assert!(chunks.last().unwrap().starts_with("\x1b_Gm=0;"));
        let payload: usize = chunks.iter().map(|c| c.split_once(';').unwrap().1.len()).sum();
        assert_eq!(payload, base64(big.as_raw()).len());
        assert!(chunks.iter().all(|c| c.split_once(';').unwrap().1.len() <= 4096));

        assert_eq!(delete(9), b"\x1b_Ga=d,d=I,i=9,q=2\x1b\\");
    }

    #[test]
    fn the_geometry_sizes_the_circle_and_covers_only_blank_cells_beside_it() {
        let g = |cell, width, fit| geometry(cell, width, fit);
        // A 10×20 cell. Height fit, width 1: a 20-pixel circle centred on the dot's cell
        // overhangs half a cell each way, so it covers the blank before and after the dot.
        let one = g((10, 20), 1, Fit::Height);
        assert_eq!((one.diameter, one.before, one.cols, one.canvas), (20, 1, 3, (30, 20)));
        assert_eq!(one.centre, (15.0, 10.0));
        // Width 2: the two cells already span the row's height; no overhang.
        let two = g((10, 20), 2, Fit::Height);
        assert_eq!((two.diameter, two.before, two.cols, two.canvas), (20, 0, 2, (20, 20)));
        assert_eq!(two.centre, (10.0, 10.0));
        // Width fit: the diameter is the cells' width, centred vertically, no overhang.
        let narrow = g((10, 20), 1, Fit::Width);
        assert_eq!(
            (narrow.diameter, narrow.before, narrow.cols, narrow.canvas),
            (10, 0, 1, (10, 20))
        );
        assert_eq!(narrow.centre, (5.0, 10.0));
        let wide = g((10, 20), 2, Fit::Width);
        assert_eq!((wide.diameter, wide.cols, wide.canvas), (20, 2, (20, 20)));
        // Width fit on a short row caps at its height, so the circle never clips top/bottom.
        assert_eq!(g((12, 16), 2, Fit::Width).diameter, 16);
        // A very tall cell: the height-fit overhang wants two cells each side but gets the one
        // blank there is; at width 2 the right side is text, so it clips at the dot cells.
        let tall = g((8, 30), 1, Fit::Height);
        assert_eq!((tall.diameter, tall.before, tall.cols, tall.canvas), (30, 1, 3, (24, 30)));
        assert_eq!(tall.centre, (12.0, 15.0));
        let tall2 = g((8, 30), 2, Fit::Height);
        assert_eq!((tall2.diameter, tall2.before, tall2.cols, tall2.canvas), (30, 1, 3, (24, 30)));
        assert_eq!(tall2.centre, (16.0, 15.0), "centred on the two avatar cells, not the canvas");
        // A wide cell needs no overhang at height fit.
        let squat = g((24, 20), 1, Fit::Height);
        assert_eq!((squat.before, squat.cols, squat.canvas), (0, 1, (24, 20)));
    }

    #[test]
    fn the_painted_circle_is_clear_outside_and_anti_aliased_at_its_edge() {
        let src = RgbaImage::from_pixel(32, 32, image::Rgba([200, 100, 50, 255]));
        let alpha = |img: &RgbaImage, x, y| img.get_pixel(x, y).0[3];
        // Width fit, one cell: a 10-pixel circle in a 10×20 canvas, clear above and below.
        let one = paint(&src, &geometry((10, 20), 1, Fit::Width));
        assert_eq!(one.dimensions(), (10, 20));
        assert_eq!(alpha(&one, 5, 10), 255, "opaque at the centre");
        assert_eq!(alpha(&one, 5, 2), 0, "nothing above the circle");
        assert_eq!(alpha(&one, 5, 18), 0, "nothing below the circle");
        assert_eq!(alpha(&one, 0, 5), 0, "the corner of its square is cut away");
        let edge = alpha(&one, 0, 10);
        assert!(edge > 0 && edge < 255, "an anti-aliased edge, not a hard step: {edge}");
        // Height fit, one cell: the circle spans the row and spills into the blanks beside.
        let tall = paint(&src, &geometry((10, 20), 1, Fit::Height));
        assert_eq!(tall.dimensions(), (30, 20));
        assert_eq!(alpha(&tall, 15, 10), 255);
        assert!(alpha(&tall, 8, 10) > 0, "into the blank before the dot");
        assert!(alpha(&tall, 21, 10) > 0, "into the blank after the dot");
        assert_eq!(alpha(&tall, 0, 0), 0);
        // Clipped against text: the circle runs to the canvas edge and stops.
        let clipped = paint(&src, &geometry((8, 30), 2, Fit::Height));
        assert_eq!(clipped.dimensions(), (24, 30));
        assert_eq!(alpha(&clipped, 23, 15), 255, "cut flat at the text cell");
    }

    #[test]
    fn decode_accepts_a_png_and_rejects_garbage() {
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(RgbaImage::new(300, 200))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let img = decode(&png).expect("a png decodes");
        assert!(img.width() <= SOURCE_MAX && img.height() <= SOURCE_MAX, "downscaled");
        assert!(decode(b"<html>not an image</html>").is_none());
    }

    #[test]
    fn the_probe_filter_swallows_exactly_the_reply_and_reads_its_verdict() {
        let now = Instant::now();
        let mut f = ProbeFilter::default();
        assert_eq!(f.feed('_', true), Fed::Pass, "no probe out: every key is the user's");
        f.start(now);
        assert!(f.waiting());
        assert_eq!(f.feed('j', false), Fed::Pass, "ordinary keys pass while waiting");
        assert_eq!(f.feed('_', true), Fed::Swallowed);
        for ch in "Gi=31;OK".chars() {
            assert_eq!(f.feed(ch, false), Fed::Swallowed);
        }
        assert_eq!(f.feed('\\', true), Fed::Swallowed);
        assert_eq!(f.answer(), Some(true));
        assert_eq!(f.feed('_', true), Fed::Pass, "answered: Alt+_ is the user's again");

        // An error reply says no graphics; silence past the window says the same.
        let mut f = ProbeFilter::default();
        f.start(now);
        for (ch, alt) in [('_', true), ('G', false), ('i', false), ('=', false), ('3', false)]
            .into_iter()
            .chain("1;ENOTSUPPORTED".chars().map(|c| (c, false)))
            .chain([('\\', true)])
        {
            f.feed(ch, alt);
        }
        assert_eq!(f.answer(), Some(false));
        let mut f = ProbeFilter::default();
        f.start(now);
        f.expire(now + PROBE_WINDOW / 2);
        assert_eq!(f.answer(), None, "still inside the window");
        f.expire(now + PROBE_WINDOW);
        assert_eq!(f.answer(), Some(false));
        assert!(!f.waiting());
    }

    #[test]
    fn a_reply_parses_only_when_it_is_the_probes() {
        assert_eq!(parse_reply("Gi=31;OK"), Some(true));
        assert_eq!(parse_reply("Gi=31;EINVAL:bad"), Some(false));
        assert_eq!(parse_reply("Gi=7;OK"), None, "another image's reply");
        assert_eq!(parse_reply("x"), None);
    }

    #[test]
    fn the_store_paints_an_id_only_once_transmitted_and_keeps_failures_as_dots() {
        let mut store = Store::default();
        assert!(store.is_new("a"));
        store.mark_requested("a");
        store.mark_requested("b");
        assert!(!store.is_new("a") && store.pending());
        store.land("a".into(), Some(RgbaImage::from_pixel(8, 8, image::Rgba([1, 2, 3, 255]))));
        store.land("b".into(), None);
        assert!(!store.pending());
        assert_eq!(store.painted_id("a"), None, "landed but not on the terminal: still a dot");
        let g1 = geometry((10, 20), 1, Fit::Height);
        let g2 = geometry((10, 20), 2, Fit::Height);
        let sent = store.transmissions(g1);
        assert!(sent.starts_with(b"\x1b_Ga=T,U=1"));
        let id = store.painted_id("a").expect("transmitted: painted");
        assert_eq!(store.painted_id("b"), None, "a failed URL stays a dot");
        assert!(store.transmissions(g1).is_empty(), "sent once");
        assert!(!store.transmissions(g2).is_empty(), "a new width re-sends");

        store.forget_sent();
        assert_eq!(store.painted_id("a"), None, "after a resize: dots until re-sent");
        assert!(!store.transmissions(g2).is_empty());
        assert_eq!(store.deletions(), delete(id));
        assert!(store.deletions().is_empty(), "deleted once");
    }

    #[test]
    fn a_stalled_download_never_blocks_a_request_or_a_poll() {
        fn stall(_: &str) -> Option<Vec<u8>> {
            std::thread::sleep(Duration::from_secs(30));
            None
        }
        let fetcher = Fetcher::spawn(stall);
        let started = Instant::now();
        for i in 0..20 {
            fetcher.request(&format!("https://example.com/{i}.png"));
        }
        assert!(fetcher.try_recv().is_none());
        assert!(started.elapsed() < Duration::from_millis(200), "requests and polls never wait");
    }
}
