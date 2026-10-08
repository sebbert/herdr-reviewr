//! Inline images in the PR conversation: `![alt](url)` and `<img>` in the description and in
//! comments, painted through the Kitty graphics layer (`crate::graphics`) that avatars use.
//!
//! The pieces stay apart so none of them can hold a frame. The markdown renderer lays an image
//! out from what the [`Store`] knows — nothing until it has landed, then its size — so a
//! pending, failed, or unpaintable image is the `⧉ alt` link it always was. The fetcher's
//! threads download (`curl`, size and time caps, a forge token only for the forge's own host),
//! decode rasters or rasterise SVGs, and pack each as a PNG once. The event loop transmits an
//! image when a frame first paints its block, keeps the terminal's share bounded
//! ([`MAX_PLACED`], [`MAX_PIXELS`]) by deleting the least recently painted, and the renderer
//! swaps the block's cells for placeholders once the terminal holds the picture.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex, OnceLock};

use image::{ImageEncoder, RgbaImage};

use crate::graphics::{self, Fetcher, Request};

/// The tallest an image block grows, in rows.
pub const MAX_ROWS: u16 = 20;

/// The most images the terminal holds for reviewr at once; past it the least recently painted
/// goes first.
pub const MAX_PLACED: usize = 32;

/// The most source pixels the terminal holds for reviewr at once, across every image.
pub const MAX_PIXELS: u64 = 24_000_000;

/// The download workers: few, so a page of screenshots never fans out a crowd of `curl`s.
const WORKERS: usize = 3;

/// The download caps: bytes, seconds, and redirects to `https` only.
const LIMITS: graphics::Limits =
    graphics::Limits { max_bytes: 10_000_000, max_time: 20, https_redirects: true };

/// A decoded image's largest side and pixel count, either way past which it is refused —
/// the decoder's guard against a decompression bomb.
const DECODE_MAX_SIDE: u32 = 12_000;
const DECODE_MAX_ALLOC: u64 = 512 * 1024 * 1024;

/// The packed raster's largest side and pixel count: past any pane, so the terminal's
/// downscale does the rest, and a bound on what one image costs to send.
const RASTER_MAX_SIDE: u32 = 2048;
const RASTER_MAX_PIXELS: u64 = 4_000_000;

/// An SVG rasterises at up to this multiple of its own size, so a badge stays sharp on a
/// dense terminal.
const SVG_SCALE: f32 = 3.0;

/// The first image id inline images use: clear of the probe's `31` and the avatars' range.
const FIRST_ID: u32 = 0x60_00_00;

/// An image ready for the terminal: the size it lays out at (its own pixels; an SVG's
/// declared size), the packed raster's pixel count, and the base64 PNG.
#[derive(Clone, Debug)]
pub struct Prepared {
    pub size: (u32, u32),
    pub pixels: u64,
    pub payload: Arc<str>,
}

/// An image block's size in cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cells {
    pub cols: u16,
    pub rows: u16,
}

/// The cells an image of `size` pixels takes — or the `width`/`height` attributes', one
/// given scales the other — at `cell` pixels per cell, within `max_cols` × `max_rows`. Never
/// upscaled past one image pixel per terminal pixel, except that the smallest image still
/// takes a whole cell: a badge keeps its own size, one row tall.
#[must_use]
// Pixel and cell counts far inside f64's exact range.
#[allow(clippy::cast_precision_loss)]
pub fn fit(
    size: (u32, u32),
    attrs: (Option<u32>, Option<u32>),
    cell: (u16, u16),
    max_cols: usize,
    max_rows: u16,
) -> Cells {
    let (w, h) = (f64::from(size.0.max(1)), f64::from(size.1.max(1)));
    let (w, h) = match attrs {
        (Some(aw), Some(ah)) if aw > 0 && ah > 0 => (f64::from(aw), f64::from(ah)),
        (Some(aw), _) if aw > 0 => (f64::from(aw), h * f64::from(aw) / w),
        (_, Some(ah)) if ah > 0 => (w * f64::from(ah) / h, f64::from(ah)),
        _ => (w, h),
    };
    let (cw, ch) = (f64::from(cell.0.max(1)), f64::from(cell.1.max(1)));
    let max_cols = max_cols.clamp(1, graphics::MAX_SPAN);
    let max_rows = usize::from(max_rows).clamp(1, graphics::MAX_SPAN);
    let scale = 1f64.min(max_cols as f64 * cw / w).min(max_rows as f64 * ch / h);
    let cols = ((w * scale / cw).round() as usize).clamp(1, max_cols);
    let rows = ((h * scale / ch).round() as usize).clamp(1, max_rows);
    Cells { cols: cols as u16, rows: rows as u16 }
}

/// What resolving an image destination needs from the PR: a GitLab project's web URL, under
/// which its `/uploads/…` paths live, and a GitHub PR's host, whose file URLs are rewritten.
#[derive(Clone, Copy, Debug, Default)]
pub struct Base<'a> {
    pub uploads: Option<&'a str>,
    pub github: Option<&'a str>,
}

/// A markdown image destination as a URL to fetch: absolute `http(s)`, a protocol-relative
/// `//host/…` as `https`, or a GitLab `/uploads/…` path under the project's web URL. A GitHub
/// file page goes to its raw file ([`github_raw`]). Anything else — a repository-relative
/// path, a `data:` URL — is not fetched.
#[must_use]
pub fn resolve(dest: &str, base: Base<'_>) -> Option<String> {
    let dest = dest.trim();
    if dest.is_empty() || dest.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let lower = dest.to_ascii_lowercase();
    let url = if lower.starts_with("https://") || lower.starts_with("http://") {
        dest.to_string()
    } else if let Some(rest) = dest.strip_prefix("//") {
        if rest.is_empty() {
            return None;
        }
        format!("https://{rest}")
    } else {
        match base.uploads {
            Some(project) if dest.starts_with("/uploads/") => {
                format!("{}{dest}", project.trim_end_matches('/'))
            }
            _ => return None,
        }
    };
    Some(base.github.and_then(|host| github_raw(&url, host)).unwrap_or(url))
}

/// A GitHub file URL on `host` as the URL that serves its bytes to a token:
/// `/<owner>/<repo>/raw/<ref>/<path>`, or `/blob/<ref>/<path>?raw=true`, becomes
/// `raw.githubusercontent.com/<owner>/<repo>/<ref>/<path>` on github.com and
/// `<host>/raw/<owner>/<repo>/<ref>/<path>` on GitHub Enterprise. The github.com forms answer
/// only a browser session, never a token. The ref and path stay one string, so GitHub
/// settles a ref with a slash in it. `None` for anything else.
#[must_use]
pub fn github_raw(url: &str, host: &str) -> Option<String> {
    let authority = graphics::https_authority(url)?;
    if host.is_empty() || !authority.eq_ignore_ascii_case(host) {
        return None;
    }
    let rest = &url["https://".len() + authority.len()..];
    let rest = rest.split_once('#').map_or(rest, |(path, _)| path);
    let (path, query) = rest.split_once('?').map_or((rest, None), |(p, q)| (p, Some(q)));
    let mut parts = path.trim_start_matches('/').splitn(4, '/');
    let (owner, repo, kind, file) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    let raw_query = query.is_some_and(|q| q.split('&').any(|kv| kv == "raw=true" || kv == "raw=1"));
    let file_page = kind == "raw" || (kind == "blob" && raw_query);
    if owner.is_empty() || repo.is_empty() || file.is_empty() || !file_page {
        return None;
    }
    Some(if host.eq_ignore_ascii_case("github.com") {
        format!("https://raw.githubusercontent.com/{owner}/{repo}/{file}")
    } else {
        format!("https://{authority}/raw/{owner}/{repo}/{file}")
    })
}

/// A GitLab merge request's project web URL — where its `/uploads/` paths live.
#[must_use]
pub fn gitlab_project_url(mr_url: &str) -> Option<&str> {
    mr_url.split_once("/-/").map(|(project, _)| project)
}

/// Whether `bytes` is an SVG document rather than a raster: an `<svg` root, possibly behind
/// an XML declaration, a doctype, or comments.
#[must_use]
pub fn is_svg(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(4096)];
    let text = String::from_utf8_lossy(head);
    let text = text.trim_start_matches('\u{feff}').trim_start();
    text.starts_with("<svg")
        || (text.starts_with("<?xml") || text.starts_with("<!")) && text.contains("<svg")
}

/// Decode or rasterise a download and pack it for the terminal, `None` for anything that is
/// not an image this can paint.
#[must_use]
pub fn prepare(bytes: &[u8]) -> Option<Prepared> {
    let (size, img) = if is_svg(bytes) { rasterize_svg(bytes)? } else { decode_raster(bytes)? };
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(
        &mut png,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::Adaptive,
    )
    .write_image(img.as_raw(), img.width(), img.height(), image::ExtendedColorType::Rgba8)
    .ok()?;
    Some(Prepared {
        size,
        pixels: u64::from(img.width()) * u64::from(img.height()),
        payload: Arc::from(graphics::base64(&png)),
    })
}

/// The scale that brings `w`×`h` within the packed raster's bounds, at most `cap`.
// Pixel counts within the raster caps, far inside f64's exact range.
#[allow(clippy::cast_precision_loss)]
fn raster_scale(w: f64, h: f64, cap: f64) -> f64 {
    cap.min(f64::from(RASTER_MAX_SIDE) / w.max(h)).min((RASTER_MAX_PIXELS as f64 / (w * h)).sqrt())
}

/// A raster download decoded (a GIF's or animated WebP's first frame), within the decode
/// limits, then scaled into the packed raster's bounds. Its own size is what it lays out at.
fn decode_raster(bytes: &[u8]) -> Option<((u32, u32), RgbaImage)> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(DECODE_MAX_SIDE);
    limits.max_image_height = Some(DECODE_MAX_SIDE);
    limits.max_alloc = Some(DECODE_MAX_ALLOC);
    reader.limits(limits);
    let img = reader.decode().ok()?;
    let size = (img.width(), img.height());
    let scale = raster_scale(f64::from(size.0), f64::from(size.1), 1.0);
    let img = if scale < 1.0 {
        let w = (f64::from(size.0) * scale).floor().max(1.0) as u32;
        let h = (f64::from(size.1) * scale).floor().max(1.0) as u32;
        img.thumbnail(w, h)
    } else {
        img
    };
    Some((size, img.to_rgba8()))
}

/// The system fonts, loaded once on a download worker the first time an SVG has text.
fn fonts() -> Arc<resvg::usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut db = resvg::usvg::fontdb::Database::new();
            db.load_system_fonts();
            Arc::new(db)
        })
        .clone()
}

/// An SVG rasterised in-process: no `<image>` of any kind (the href resolvers refuse every
/// file, URL, and data URL), so an SVG never reads a file or reaches the network; no
/// scripts, which the renderer never runs. It lays out at its declared size and rasterises at
/// up to [`SVG_SCALE`] times that, within the packed raster's bounds.
fn rasterize_svg(bytes: &[u8]) -> Option<((u32, u32), RgbaImage)> {
    use resvg::usvg;
    let has_text = bytes.windows(5).any(|w| w == b"<text");
    let opts = usvg::Options {
        resources_dir: None,
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        fontdb: if has_text { fonts() } else { Arc::new(usvg::fontdb::Database::new()) },
        ..usvg::Options::default()
    };
    let tree = usvg::Tree::from_data(bytes, &opts).ok()?;
    let (w, h) = (f64::from(tree.size().width()), f64::from(tree.size().height()));
    if !(w.is_finite() && h.is_finite() && w >= 1.0 && h >= 1.0) {
        return None;
    }
    let size = (
        w.ceil().min(f64::from(DECODE_MAX_SIDE)) as u32,
        h.ceil().min(f64::from(DECODE_MAX_SIDE)) as u32,
    );
    let scale = raster_scale(w, h, f64::from(SVG_SCALE));
    let (pw, ph) = ((w * scale).ceil().max(1.0) as u32, (h * scale).ceil().max(1.0) as u32);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(pw, ph)?;
    let scale = scale as f32;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let mut raw = Vec::with_capacity(pixmap.pixels().len() * 4);
    for px in pixmap.pixels() {
        let c = px.demultiply();
        raw.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Some((size, RgbaImage::from_raw(pw, ph, raw)?))
}

/// The forge tokens the workers have read, by host: read once each, off the frame loop.
#[derive(Debug, Default)]
struct Tokens(Mutex<HashMap<String, Option<String>>>);

impl Tokens {
    fn get(&self, host: &str, read: fn(&str) -> Option<String>) -> Option<String> {
        if let Some(known) = self.0.lock().ok()?.get(host) {
            return known.clone();
        }
        let token = read(host);
        self.0.lock().ok()?.insert(host.to_string(), token.clone());
        token
    }
}

/// Spawn the inline-image workers: download (with the forge token when the request names its
/// host), then [`prepare`].
#[must_use]
pub fn spawn_fetcher() -> Fetcher<Prepared> {
    let tokens = Arc::new(Tokens::default());
    Fetcher::spawn_with(WORKERS, move |r: &Request| {
        let token = r.token_host.as_deref().and_then(|h| tokens.get(h, graphics::gh_token));
        graphics::curl(&r.url, LIMITS, token.as_deref()).as_deref().and_then(prepare)
    })
}

/// One size of one image on the terminal: its id, whether the terminal holds it now, and
/// the frame that last painted it.
#[derive(Debug)]
struct Variant {
    id: u32,
    placed: bool,
    painted: u64,
}

/// One URL's session state.
#[derive(Debug)]
enum Entry {
    Pending,
    Failed,
    Ready { prepared: Prepared, variants: HashMap<Cells, Variant> },
}

/// Every inline image this session has asked for, keyed by URL. A failed URL stays its alt
/// link for the session, so a dead host is asked once. Each size an image paints at is its
/// own terminal image, so the placeholder cells need no placement id.
#[derive(Debug, Default)]
pub struct Store {
    entries: HashMap<String, Entry>,
    next_id: u32,
    clock: u64,
}

impl Store {
    /// The size `url` lays out at, once it has landed.
    #[must_use]
    pub fn size(&self, url: &str) -> Option<(u32, u32)> {
        match self.entries.get(url) {
            Some(Entry::Ready { prepared, .. }) => Some(prepared.size),
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

    /// Land one download: a prepared image becomes ready, a failure stays the alt link.
    pub fn land(&mut self, url: String, prepared: Option<Prepared>) {
        let entry = match prepared {
            Some(prepared) => Entry::Ready { prepared, variants: HashMap::new() },
            None => Entry::Failed,
        };
        self.entries.insert(url, entry);
    }

    /// The image id to paint `url` at `cells` with: only once the terminal holds it.
    #[must_use]
    pub fn placed_id(&self, url: &str, cells: Cells) -> Option<u32> {
        match self.entries.get(url) {
            Some(Entry::Ready { variants, .. }) => {
                variants.get(&cells).filter(|v| v.placed).map(|v| v.id)
            }
            _ => None,
        }
    }

    /// The writes that put every image this frame painted on the terminal: a transmission for
    /// each not there yet, after deleting the least recently painted others while the budget
    /// would overflow. An image that still does not fit stays its alt text. Returns the bytes
    /// and whether anything new went out (the frame then repaints with it).
    pub fn place(&mut self, painted: &[(String, Cells)]) -> (Vec<u8>, bool) {
        self.clock += 1;
        let clock = self.clock;
        let mut wanted = Vec::new();
        for (url, cells) in painted {
            let Some(Entry::Ready { prepared, variants }) = self.entries.get_mut(url) else {
                continue;
            };
            let next = &mut self.next_id;
            let v = variants.entry(*cells).or_insert_with(|| {
                *next += 1;
                Variant { id: FIRST_ID + *next - 1, placed: false, painted: 0 }
            });
            v.painted = clock;
            if !v.placed && !wanted.iter().any(|(u, c, _)| u == url && c == cells) {
                wanted.push((url.clone(), *cells, prepared.pixels));
            }
        }
        let mut out = Vec::new();
        let mut sent = false;
        for (url, cells, pixels) in wanted {
            if !self.make_room(pixels, clock, &mut out) {
                continue;
            }
            let Some(Entry::Ready { prepared, variants }) = self.entries.get_mut(&url) else {
                continue;
            };
            let Some(v) = variants.get_mut(&cells) else { continue };
            let head = format!("a=T,U=1,f=100,i={},c={},r={},q=2,", v.id, cells.cols, cells.rows);
            out.extend(graphics::chunked(&head, &prepared.payload));
            v.placed = true;
            sent = true;
        }
        (out, sent)
    }

    /// The terminal's current share: images held and their pixels.
    fn held(&self) -> (usize, u64) {
        let mut held = (0, 0);
        for entry in self.entries.values() {
            if let Entry::Ready { prepared, variants } = entry {
                let placed = variants.values().filter(|v| v.placed).count();
                held.0 += placed;
                held.1 += prepared.pixels * placed as u64;
            }
        }
        held
    }

    /// Delete the least recently painted images, never one painted this frame, until one
    /// more of `pixels` fits the budget. Whether it fits.
    fn make_room(&mut self, pixels: u64, clock: u64, out: &mut Vec<u8>) -> bool {
        loop {
            let (count, total) = self.held();
            if count < MAX_PLACED && total + pixels <= MAX_PIXELS {
                return true;
            }
            let oldest = self
                .entries
                .values_mut()
                .filter_map(|e| match e {
                    Entry::Ready { variants, .. } => Some(variants.values_mut()),
                    _ => None,
                })
                .flatten()
                .filter(|v| v.placed && v.painted < clock)
                .min_by_key(|v| v.painted);
            let Some(v) = oldest else { return false };
            v.placed = false;
            out.extend(graphics::delete(v.id));
        }
    }

    /// The deletions for every image on the terminal, which then counts as not there — the
    /// next frame that paints one puts it back.
    pub fn deletions(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        for entry in self.entries.values_mut() {
            if let Entry::Ready { variants, .. } = entry {
                for v in variants.values_mut().filter(|v| v.placed) {
                    v.placed = false;
                    out.extend(graphics::delete(v.id));
                }
            }
        }
        out
    }

    /// Forget what the terminal holds, after anything that could have dropped it (a resize):
    /// the next paint re-transmits.
    pub fn forget_sent(&mut self) {
        for entry in self.entries.values_mut() {
            if let Entry::Ready { variants, .. } = entry {
                for v in variants.values_mut() {
                    v.placed = false;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(RgbaImage::from_pixel(
            w,
            h,
            image::Rgba([9, 99, 199, 255]),
        ))
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
        out
    }

    fn ready(size: (u32, u32), pixels: u64) -> Prepared {
        Prepared { size, pixels, payload: Arc::from("QUJD") }
    }

    #[test]
    fn an_image_fits_its_pixels_the_pane_and_the_row_cap() {
        let cell = (10, 20);
        let c = |cols, rows| Cells { cols, rows };
        // 1:1 within the room: 400×200 px is 40×10 cells.
        assert_eq!(fit((400, 200), (None, None), cell, 80, 20), c(40, 10));
        // Wider than the pane: scaled down to its width, aspect kept.
        assert_eq!(fit((1600, 400), (None, None), cell, 80, 20), c(80, 10));
        // Taller than the cap: scaled down to 20 rows.
        assert_eq!(fit((400, 1600), (None, None), cell, 80, 20), c(10, 20));
        // Never upscaled: a 30×30 icon is 3×2 cells, not the pane.
        assert_eq!(fit((30, 30), (None, None), cell, 80, 20), c(3, 2));
        // A badge keeps its size, one row tall; a speck still takes a cell.
        assert_eq!(fit((90, 20), (None, None), cell, 80, 20), c(9, 1));
        assert_eq!(fit((2, 2), (None, None), cell, 80, 20), c(1, 1));
        // On a dense terminal a badge is still one row, not a fraction.
        assert_eq!(fit((90, 20), (None, None), (16, 34), 80, 20), c(6, 1));
        // `width`/`height` attributes: both, or one scaling the other.
        assert_eq!(fit((2000, 1000), (Some(300), None), cell, 80, 20), c(30, 8));
        assert_eq!(fit((2000, 1000), (None, Some(100)), cell, 80, 20), c(20, 5));
        assert_eq!(fit((2000, 1000), (Some(100), Some(100)), cell, 80, 20), c(10, 5));
        assert_eq!(fit((2000, 1000), (Some(0), None), cell, 80, 20), c(80, 20));
        // Degenerate input never panics or yields an empty block.
        assert_eq!(fit((0, 0), (None, None), (0, 0), 0, 0), c(1, 1));
    }

    #[test]
    fn github_file_pages_resolve_to_the_raw_file_a_token_can_fetch() {
        let gh = Base { uploads: None, github: Some("github.com") };
        let diagram_bot = "https://github.com/o/r/raw/diagrams/7/abc/c-light-1f.svg";
        assert_eq!(
            resolve(diagram_bot, gh).as_deref(),
            Some("https://raw.githubusercontent.com/o/r/diagrams/7/abc/c-light-1f.svg")
        );
        for (url, want) in [
            (
                "https://github.com/o/r/blob/main/docs/a.png?raw=true",
                Some("https://raw.githubusercontent.com/o/r/main/docs/a.png"),
            ),
            (
                "https://github.com/o/r/blob/feature/x/a.png?foo=1&raw=1#frag",
                Some("https://raw.githubusercontent.com/o/r/feature/x/a.png"),
            ),
            (
                "https://GitHub.com/o/r/raw/main/a.png",
                Some("https://raw.githubusercontent.com/o/r/main/a.png"),
            ),
            ("https://github.com/o/r/blob/main/a.png", None),
            ("https://github.com/o/r/raw/", None),
            ("https://github.com/user-attachments/assets/abc", None),
            ("https://github.com/o/r/pull/3", None),
            ("http://github.com/o/r/raw/main/a.png", None),
            ("https://github.com.evil.example/o/r/raw/main/a.png", None),
            ("https://evil.example/o/r/raw/main/a.png", None),
        ] {
            assert_eq!(github_raw(url, "github.com").as_deref(), want, "{url}");
        }
        // GitHub Enterprise serves raw files on its own host.
        assert_eq!(
            github_raw("https://ghe.corp/o/r/raw/main/a.svg", "ghe.corp").as_deref(),
            Some("https://ghe.corp/raw/o/r/main/a.svg")
        );
        assert_eq!(github_raw("https://github.com/o/r/raw/main/a.svg", "ghe.corp"), None);
        // Not a GitHub PR: no rewrite.
        assert_eq!(resolve(diagram_bot, Base::default()).as_deref(), Some(diagram_bot));
    }

    #[test]
    fn destinations_resolve_to_fetchable_urls_only() {
        let up = Base { uploads: Some("https://gitlab.com/g/p"), github: None };
        let none = Base::default();
        assert_eq!(
            resolve("https://a.example/x.png", none).as_deref(),
            Some("https://a.example/x.png")
        );
        assert_eq!(
            resolve(" http://a.example/x.png ", none).as_deref(),
            Some("http://a.example/x.png")
        );
        assert_eq!(
            resolve("//cdn.example/x.svg", none).as_deref(),
            Some("https://cdn.example/x.svg")
        );
        assert_eq!(
            resolve("/uploads/abc/shot.png", up).as_deref(),
            Some("https://gitlab.com/g/p/uploads/abc/shot.png")
        );
        for dest in [
            "/uploads/abc/shot.png",
            "docs/a.png",
            "data:image/png;base64,AAAA",
            "",
            "file:///etc/passwd",
            "javascript:x",
            "https://a.example/a b.png",
        ] {
            assert_eq!(resolve(dest, none), None, "{dest}");
        }
        assert_eq!(resolve("docs/a.png", up), None);
        assert_eq!(
            gitlab_project_url("https://gitlab.com/g/p/-/merge_requests/3"),
            Some("https://gitlab.com/g/p")
        );
        assert_eq!(gitlab_project_url("https://gitlab.com/g/p"), None);
    }

    #[test]
    fn a_raster_decodes_at_its_own_size_and_packs_as_png() {
        let p = prepare(&png(300, 120)).expect("a png prepares");
        assert_eq!(p.size, (300, 120));
        assert_eq!(p.pixels, 300 * 120);
        assert!(p.payload.starts_with("iVBORw0KGgo"), "a base64 PNG");
        // A huge image lays out at its own size but packs within the raster bounds.
        let big = prepare(&png(4000, 1000)).unwrap();
        assert_eq!(big.size, (4000, 1000));
        assert!(big.pixels <= RASTER_MAX_PIXELS);
        assert!(prepare(b"<html>not an image</html>").is_none());
        assert!(prepare(b"").is_none());
    }

    #[test]
    fn a_small_svg_rasterises_in_process_at_its_declared_size() {
        let svg = br##"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="90" height="20">
  <rect width="90" height="20" rx="3" fill="#4c1"/>
  <rect width="40" height="20" fill="#555"/>
  <image href="file:///etc/hosts" width="10" height="10"/>
  <image xlink:href="https://example.com/x.png" width="10" height="10"/>
</svg>"##;
        assert!(is_svg(svg));
        assert!(!is_svg(&png(2, 2)));
        let (size, img) = rasterize_svg(svg).expect("rasterises");
        assert_eq!(size, (90, 20));
        assert_eq!(img.dimensions(), (270, 60), "rasterised at three times for a sharp badge");
        let px = img.get_pixel(200, 30).0;
        assert_eq!(px, [0x44, 0xcc, 0x11, 255], "the green half, straight alpha");
        assert_eq!(img.get_pixel(30, 30).0, [0x55, 0x55, 0x55, 255]);
        assert_eq!(prepare(svg).unwrap().size, (90, 20));
        assert!(rasterize_svg(b"<svg").is_none(), "malformed");
    }

    #[test]
    fn the_store_places_only_painted_images_within_its_budget() {
        let mut store = Store::default();
        let cells = Cells { cols: 9, rows: 1 };
        assert!(store.is_new("a"));
        store.mark_requested("a");
        store.mark_requested("b");
        assert!(store.pending() && store.size("a").is_none());
        store.land("a".into(), Some(ready((90, 20), 5400)));
        store.land("b".into(), None);
        assert!(!store.pending());
        assert_eq!(store.size("a"), Some((90, 20)));
        assert_eq!(store.size("b"), None, "a failure stays its alt link");
        assert_eq!(store.placed_id("a", cells), None, "landed but not on the terminal");

        let (bytes, sent) = store.place(&[("a".into(), cells), ("b".into(), cells)]);
        assert!(sent);
        let seq = String::from_utf8(bytes).unwrap();
        assert!(seq.starts_with("\x1b_Ga=T,U=1,f=100,i=6291456,c=9,r=1,q=2,m=0;QUJD"), "{seq}");
        let id = store.placed_id("a", cells).unwrap();
        assert_eq!(store.place(&[("a".into(), cells)]), (Vec::new(), false), "sent once");
        // Another size of the same image is its own terminal image.
        let wide = Cells { cols: 18, rows: 2 };
        assert!(store.place(&[("a".into(), wide)]).1);
        assert_ne!(store.placed_id("a", wide), Some(id));

        store.forget_sent();
        assert_eq!(store.placed_id("a", cells), None, "after a resize: alt until re-sent");
        assert!(store.place(&[("a".into(), cells)]).1);
        assert_eq!(store.deletions(), graphics::delete(id));
        assert!(store.deletions().is_empty(), "deleted once");
    }

    #[test]
    fn past_the_budget_the_least_recently_painted_image_is_deleted_first() {
        let mut store = Store::default();
        let cells = Cells { cols: 4, rows: 2 };
        let big = MAX_PIXELS / 2;
        for url in ["a", "b", "c"] {
            store.land(url.into(), Some(ready((40, 40), big)));
        }
        store.place(&[("a".into(), cells)]);
        store.place(&[("b".into(), cells)]);
        let a = store.placed_id("a", cells).unwrap();
        // `c` overflows the pixels: `a`, painted longest ago, goes.
        let (bytes, sent) = store.place(&[("c".into(), cells), ("b".into(), cells)]);
        assert!(sent);
        assert!(bytes.starts_with(&graphics::delete(a)));
        assert_eq!(store.placed_id("a", cells), None);
        assert!(store.placed_id("b", cells).is_some() && store.placed_id("c", cells).is_some());
        // Everything on screen this frame stays: a third that cannot fit keeps its alt.
        let (_, sent) =
            store.place(&[("a".into(), cells), ("b".into(), cells), ("c".into(), cells)]);
        assert!(!sent);
        assert_eq!(store.placed_id("a", cells), None);

        // The count cap holds the same way.
        let mut store = Store::default();
        let painted: Vec<_> = (0..=MAX_PLACED)
            .map(|i| {
                store.land(i.to_string(), Some(ready((10, 10), 100)));
                (i.to_string(), cells)
            })
            .collect();
        for one in &painted {
            store.place(std::slice::from_ref(one));
        }
        assert_eq!(store.held().0, MAX_PLACED);
        assert_eq!(store.placed_id("0", cells), None, "the oldest made room");
    }

    #[test]
    fn a_token_is_read_once_per_host() {
        static READS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        fn read(host: &str) -> Option<String> {
            READS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            (host == "github.com").then(|| "gho_x".to_string())
        }
        let tokens = Tokens::default();
        assert_eq!(tokens.get("github.com", read).as_deref(), Some("gho_x"));
        assert_eq!(tokens.get("github.com", read).as_deref(), Some("gho_x"));
        assert_eq!(tokens.get("ghe.corp", read), None);
        assert_eq!(tokens.get("ghe.corp", read), None);
        assert_eq!(READS.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
}
