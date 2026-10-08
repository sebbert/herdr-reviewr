//! Inline images in the PR conversation: `![alt](url)` and `<img>` in the description and in
//! comments, painted through the Kitty graphics layer (`crate::graphics`) that avatars use.
//!
//! The pieces stay apart so none of them can hold a frame. The markdown renderer lays an image
//! out from what the [`Store`] knows — nothing until it has landed, then its size — so a
//! pending, failed, or unpaintable image is the `⧉ alt` link it always was. The fetcher's
//! threads download (`curl`, size and time caps, a forge token only for the forge's own host),
//! decode rasters or rasterise SVGs, and pack each as a PNG once. The event loop transmits an
//! image once, when a frame first paints its block, and from then on only moves its one
//! virtual placement: the terminal fits the picture into whatever cells the block has, aspect
//! kept, so a resize or a wider pane sends a few bytes, never the picture. Only an SVG drawn
//! larger than its packed raster is re-rasterised at the block's pixel size, on the
//! [`Rasterizer`]'s thread, once resizes go quiet, the old raster scaled meanwhile. The
//! terminal's share stays bounded ([`MAX_PLACED`], [`MAX_PIXELS`]) by deleting the least
//! recently painted, and the renderer swaps the block's cells for placeholders once the
//! terminal holds the picture.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use image::{ImageEncoder, RgbaImage};

use crate::graphics::{self, Fetcher, Request};

/// The tallest an image block grows, in rows, unless `inline_image_max_rows` says otherwise.
pub const DEFAULT_MAX_ROWS: u16 = 40;

/// The most pixels a sharp SVG raster is drawn at: past it the terminal scales up.
const SHARP_MAX_PIXELS: u64 = 6_000_000;

/// How long an SVG's block must keep one pixel size before it is re-rasterised at it: a
/// drag-resize asks once, after it settles.
pub const QUIET: Duration = Duration::from_millis(150);

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
/// declared size), the packed raster's pixel size and count, the base64 PNG, and for an SVG
/// its source, which a larger block re-rasterises from.
#[derive(Clone, Debug)]
pub struct Prepared {
    pub size: (u32, u32),
    pub raster: (u32, u32),
    pub pixels: u64,
    pub payload: Arc<str>,
    pub svg: Option<Arc<[u8]>>,
}

/// How a block image (one taller than a row) is sized: across the box's whole width, or at
/// its own size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Width {
    #[default]
    Fill,
    Native,
}

impl Width {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fill => "fill",
            Self::Native => "native",
        }
    }
}

/// The row cap `inline_image_max_rows` names: `0` is none (the protocol's own limit).
#[must_use]
pub fn row_cap(max_rows: u16) -> u16 {
    if max_rows == 0 { graphics::MAX_SPAN as u16 } else { max_rows }
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
    scaled(size, attrs, cell, max_cols, max_rows, 1.0)
}

/// The cells a block image takes across the whole `max_cols`, aspect kept, unless that would
/// pass `max_rows` — then as wide as `max_rows` allows.
#[must_use]
pub fn fill(
    size: (u32, u32),
    attrs: (Option<u32>, Option<u32>),
    cell: (u16, u16),
    max_cols: usize,
    max_rows: u16,
) -> Cells {
    scaled(size, attrs, cell, max_cols, max_rows, f64::INFINITY)
}

/// The cells an image takes under `width`: a badge (one row at its own size) always keeps its
/// own size and rides its line; a block image fills or keeps its own size.
#[must_use]
pub fn layout(
    size: (u32, u32),
    attrs: (Option<u32>, Option<u32>),
    cell: (u16, u16),
    max_cols: usize,
    max_rows: u16,
    width: Width,
) -> Cells {
    let native = fit(size, attrs, cell, max_cols, max_rows);
    if native.rows <= 1 || width == Width::Native {
        native
    } else {
        fill(size, attrs, cell, max_cols, max_rows)
    }
}

/// The cells of `size` (or its attributes) scaled by up to `most` pixels per pixel, within
/// `max_cols` × `max_rows`, aspect kept, at least one cell.
// Pixel and cell counts far inside f64's exact range.
#[allow(clippy::cast_precision_loss)]
fn scaled(
    size: (u32, u32),
    attrs: (Option<u32>, Option<u32>),
    cell: (u16, u16),
    max_cols: usize,
    max_rows: u16,
    most: f64,
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
    let scale = most.min(max_cols as f64 * cw / w).min(max_rows as f64 * ch / h);
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
    let svg = is_svg(bytes);
    let (size, img) = if svg { rasterize_svg(bytes)? } else { decode_raster(bytes)? };
    let (payload, pixels) = pack(&img)?;
    Some(Prepared {
        size,
        raster: img.dimensions(),
        pixels,
        payload,
        svg: svg.then(|| Arc::from(bytes)),
    })
}

/// `img` as the terminal takes it: a base64 PNG, and its pixel count.
fn pack(img: &RgbaImage) -> Option<(Arc<str>, u64)> {
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(
        &mut png,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::Adaptive,
    )
    .write_image(img.as_raw(), img.width(), img.height(), image::ExtendedColorType::Rgba8)
    .ok()?;
    let pixels = u64::from(img.width()) * u64::from(img.height());
    Some((Arc::from(graphics::base64(&png)), pixels))
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
    let tree = svg_tree(bytes)?;
    let (w, h) = (f64::from(tree.size().width()), f64::from(tree.size().height()));
    let size = (
        w.ceil().min(f64::from(DECODE_MAX_SIDE)) as u32,
        h.ceil().min(f64::from(DECODE_MAX_SIDE)) as u32,
    );
    let img = render_svg(&tree, raster_scale(w, h, f64::from(SVG_SCALE)))?;
    Some((size, img))
}

/// An SVG rasterised to fit `px`, aspect kept — a block's own pixel size — and packed.
#[must_use]
pub fn rasterize_svg_to(bytes: &[u8], px: (u32, u32)) -> Option<(Arc<str>, u64)> {
    let tree = svg_tree(bytes)?;
    let (w, h) = (f64::from(tree.size().width()), f64::from(tree.size().height()));
    let scale = (f64::from(px.0.max(1)) / w).min(f64::from(px.1.max(1)) / h);
    pack(&render_svg(&tree, scale)?)
}

/// The parsed SVG, its declared size sane.
fn svg_tree(bytes: &[u8]) -> Option<resvg::usvg::Tree> {
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
    (w.is_finite() && h.is_finite() && w >= 1.0 && h >= 1.0).then_some(tree)
}

/// `tree` drawn at `scale`, straight alpha.
fn render_svg(tree: &resvg::usvg::Tree, scale: f64) -> Option<RgbaImage> {
    let (w, h) = (f64::from(tree.size().width()), f64::from(tree.size().height()));
    let (pw, ph) = ((w * scale).ceil().max(1.0) as u32, (h * scale).ceil().max(1.0) as u32);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(pw, ph)?;
    let scale = scale as f32;
    resvg::render(
        tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let mut raw = Vec::with_capacity(pixmap.pixels().len() * 4);
    for px in pixmap.pixels() {
        let c = px.demultiply();
        raw.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    RgbaImage::from_raw(pw, ph, raw)
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

/// One sharp SVG raster to draw: the image, its source, the pixel size, and the tag that
/// names this ask — a later ask for the same image supersedes it.
#[derive(Clone, Debug)]
pub struct RasterJob {
    pub url: String,
    pub svg: Arc<[u8]>,
    pub px: (u32, u32),
    pub tag: u64,
}

/// One drawn raster: the job's image, size, and tag, and the packed PNG with its pixel count,
/// `None` when it failed.
pub type RasterDone = (String, (u32, u32), u64, Option<(Arc<str>, u64)>);

/// The SVG re-rasteriser: one thread drawing jobs in order. Asks never block, and a landing
/// counts only while its tag is the image's latest ask.
#[derive(Debug)]
pub struct Rasterizer {
    jobs: mpsc::Sender<RasterJob>,
    done: mpsc::Receiver<RasterDone>,
}

impl Rasterizer {
    #[must_use]
    pub fn spawn() -> Self {
        let (jobs, job_rx) = mpsc::channel::<RasterJob>();
        let (done_tx, done) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(job) = job_rx.recv() {
                let out = rasterize_svg_to(&job.svg, job.px);
                if done_tx.send((job.url, job.px, job.tag, out)).is_err() {
                    return;
                }
            }
        });
        Self { jobs, done }
    }

    pub fn request(&self, job: RasterJob) {
        let _ = self.jobs.send(job);
    }

    pub fn try_recv(&self) -> Option<RasterDone> {
        self.done.try_recv().ok()
    }
}

/// Which raster the terminal holds under an image's id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Held {
    /// The raster packed at download.
    Packed,
    /// A sharp SVG raster drawn at this pixel size.
    Sharp((u32, u32)),
}

/// One image on (or off) the terminal: its id, what the terminal holds, the cells its one
/// virtual placement spans, the frame that last painted it, and for an SVG the sharp
/// raster's state.
#[derive(Debug)]
struct Shown {
    id: u32,
    held: Option<Held>,
    placed: Option<Cells>,
    painted: u64,
    /// The latest sharp raster that landed: its pixel size, payload, and pixel count.
    sharp: Option<((u32, u32), Arc<str>, u64)>,
    /// The sharp size the block wants and since when — the resize debounce.
    want: Option<((u32, u32), Instant)>,
    /// The sharp raster last asked for and its tag.
    asked: Option<((u32, u32), u64)>,
}

/// One URL's session state.
#[derive(Debug)]
enum Entry {
    Pending,
    Failed,
    Ready { prepared: Prepared, shown: Shown },
}

/// What one [`Store::place`] pass wrote and asked for.
#[derive(Debug, Default)]
pub struct Placed {
    /// The terminal writes: transmissions, deletions, placements.
    pub bytes: Vec<u8>,
    /// A picture went out for the first time: the next frame paints it.
    pub sent: bool,
    /// Sharp SVG rasters to draw.
    pub rasters: Vec<RasterJob>,
}

/// Every inline image this session has asked for, keyed by URL. A failed URL stays its alt
/// link for the session, so a dead host is asked once. Each image is one terminal image with
/// one virtual placement: its pixels go out once, and a new footprint moves only the
/// placement, which the terminal fits the picture into, aspect kept.
#[derive(Debug, Default)]
pub struct Store {
    entries: HashMap<String, Entry>,
    next_id: u32,
    clock: u64,
    tag: u64,
}

/// The pixel size a sharp raster of `size` is drawn at for `cells`: the block's pixels with
/// the image fitted in, aspect kept, within [`SHARP_MAX_PIXELS`].
// Pixel counts far inside f64's exact range.
#[allow(clippy::cast_precision_loss)]
fn sharp_px(size: (u32, u32), cells: Cells, cell: (u16, u16)) -> (u32, u32) {
    let (w, h) = (f64::from(size.0.max(1)), f64::from(size.1.max(1)));
    let bw = f64::from(cells.cols) * f64::from(cell.0.max(1));
    let bh = f64::from(cells.rows) * f64::from(cell.1.max(1));
    let scale = (bw / w).min(bh / h).min((SHARP_MAX_PIXELS as f64 / (w * h)).sqrt());
    ((w * scale).round().max(1.0) as u32, (h * scale).round().max(1.0) as u32)
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

    /// Whether a sharp raster is still to come: one asked for, or one waiting out
    /// [`QUIET`] — so the loop keeps waking for it.
    #[must_use]
    pub fn sharp_pending(&self) -> bool {
        self.entries.values().any(|e| match e {
            Entry::Ready { shown, .. } => {
                shown.asked.is_some()
                    || shown.want.is_some_and(|(px, _)| {
                        shown.sharp.as_ref().is_none_or(|(have, ..)| *have != px)
                    })
            }
            _ => false,
        })
    }

    /// Land one download: a prepared image becomes ready, a failure stays the alt link.
    pub fn land(&mut self, url: String, prepared: Option<Prepared>) {
        let entry = match prepared {
            Some(prepared) => {
                self.next_id += 1;
                let shown = Shown {
                    id: FIRST_ID + self.next_id - 1,
                    held: None,
                    placed: None,
                    painted: 0,
                    sharp: None,
                    want: None,
                    asked: None,
                };
                Entry::Ready { prepared, shown }
            }
            None => Entry::Failed,
        };
        self.entries.insert(url, entry);
    }

    /// Land one sharp raster — only the image's latest ask counts. A failure keeps the
    /// packed raster, and that size is not asked for again.
    pub fn land_sharp(&mut self, done: RasterDone) {
        let (url, px, tag, out) = done;
        let Some(Entry::Ready { shown, .. }) = self.entries.get_mut(&url) else { return };
        if shown.asked != Some((px, tag)) {
            return;
        }
        shown.asked = None;
        match out {
            Some((payload, pixels)) => shown.sharp = Some((px, payload, pixels)),
            None => shown.want = None,
        }
    }

    /// The image id to paint `url` with: once the terminal holds it. Its placement may lag
    /// a footprint change by one frame, in which the terminal shows the picture at its old
    /// cells' fit — stale for a frame, never blank.
    #[must_use]
    pub fn placed_id(&self, url: &str) -> Option<u32> {
        match self.entries.get(url) {
            Some(Entry::Ready { shown, .. }) => shown.held.map(|_| shown.id),
            _ => None,
        }
    }

    /// Put every image this frame painted on the terminal at the cells it painted, at `cell`
    /// pixels per cell, `now`. An image not there yet is transmitted (after deleting the least
    /// recently painted others while the budget would overflow; one that still does not fit
    /// stays its alt text). One already there only has its placement moved when its cells
    /// changed — a few bytes. An SVG whose block outgrew its packed raster asks for a sharp
    /// one at the block's pixel size once that size has held for [`QUIET`], and swaps it in
    /// when it lands, replacing (and so freeing) the old raster.
    pub fn place(&mut self, painted: &[(String, Cells)], cell: (u16, u16), now: Instant) -> Placed {
        self.clock += 1;
        let clock = self.clock;
        let mut out = Placed::default();
        // Everything on screen this frame is painted before any of it makes room.
        for (url, _) in painted {
            if let Some(Entry::Ready { shown, .. }) = self.entries.get_mut(url) {
                shown.painted = clock;
            }
        }
        let mut seen: Vec<&str> = Vec::new();
        for (url, cells) in painted {
            // One placement per image: the first block that painted it this frame sizes it.
            if seen.contains(&url.as_str()) {
                continue;
            }
            seen.push(url);
            let Some(Entry::Ready { prepared, shown }) = self.entries.get_mut(url) else {
                continue;
            };
            if prepared.svg.is_some() {
                let px = sharp_px(prepared.size, *cells, cell);
                let outgrown = px.0 > prepared.raster.0 || px.1 > prepared.raster.1;
                if !outgrown {
                    shown.want = None;
                } else if shown.want.is_none_or(|(w, _)| w != px) {
                    shown.want = Some((px, now));
                }
            }
            // The best raster on hand: a sharp one for the wanted size, else what is held,
            // else the newest sharp one, else the packed one.
            let wanted = shown.want.map(|(px, _)| px);
            let fresh = shown.sharp.as_ref().filter(|(px, ..)| Some(*px) == wanted);
            let newest = shown.sharp.as_ref().map(|(px, ..)| Held::Sharp(*px));
            let best = fresh
                .map(|(px, ..)| Held::Sharp(*px))
                .or(shown.held)
                .or(newest)
                .unwrap_or(Held::Packed);
            if shown.held != Some(best) {
                let (payload, pixels) = match best {
                    Held::Packed => (prepared.payload.clone(), prepared.pixels),
                    Held::Sharp(_) => {
                        let (_, payload, pixels) = shown.sharp.clone().unwrap_or_default();
                        (payload, pixels)
                    }
                };
                let replacing = shown.held.is_some();
                let id = shown.id;
                if !replacing && !self.make_room(pixels, clock, &mut out.bytes) {
                    continue;
                }
                let Some(Entry::Ready { shown, .. }) = self.entries.get_mut(url) else {
                    continue;
                };
                if replacing {
                    out.bytes.extend(graphics::delete(id));
                } else {
                    out.sent = true;
                }
                let head = format!("a=t,f=100,i={id},q=2,");
                out.bytes.extend(graphics::chunked(&head, &payload));
                shown.held = Some(best);
                shown.placed = None;
            }
            let Some(Entry::Ready { prepared, shown }) = self.entries.get_mut(url) else {
                continue;
            };
            if shown.placed != Some(*cells) {
                let place = format!(
                    "\x1b_Ga=p,U=1,i={},p=1,c={},r={},q=2\x1b\\",
                    shown.id, cells.cols, cells.rows
                );
                out.bytes.extend(place.as_bytes());
                shown.placed = Some(*cells);
            }
            if let (Some(svg), Some((px, since))) = (&prepared.svg, shown.want) {
                let have = shown.sharp.as_ref().is_some_and(|(p, ..)| *p == px);
                let asked = shown.asked.is_some_and(|(p, _)| p == px);
                if !have && !asked && now.duration_since(since) >= QUIET {
                    self.tag += 1;
                    shown.asked = Some((px, self.tag));
                    let job = RasterJob { url: url.clone(), svg: svg.clone(), px, tag: self.tag };
                    out.rasters.push(job);
                }
            }
        }
        out
    }

    /// The terminal's current share: images held and their pixels.
    fn held(&self) -> (usize, u64) {
        let mut held = (0, 0);
        for entry in self.entries.values() {
            if let Entry::Ready { prepared, shown } = entry {
                match shown.held {
                    Some(Held::Packed) => held = (held.0 + 1, held.1 + prepared.pixels),
                    Some(Held::Sharp(_)) => {
                        let pixels = shown.sharp.as_ref().map_or(prepared.pixels, |s| s.2);
                        held = (held.0 + 1, held.1 + pixels);
                    }
                    None => {}
                }
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
                    Entry::Ready { shown, .. } => Some(shown),
                    _ => None,
                })
                .filter(|s| s.held.is_some() && s.painted < clock)
                .min_by_key(|s| s.painted);
            let Some(s) = oldest else { return false };
            s.held = None;
            s.placed = None;
            out.extend(graphics::delete(s.id));
        }
    }

    /// The deletions for every image on the terminal, which then counts as not there — the
    /// next frame that paints one puts it back.
    pub fn deletions(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        for entry in self.entries.values_mut() {
            if let Entry::Ready { shown, .. } = entry
                && shown.held.take().is_some()
            {
                shown.placed = None;
                out.extend(graphics::delete(shown.id));
            }
        }
        out
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
        Prepared { size, raster: size, pixels, payload: Arc::from("QUJD"), svg: None }
    }

    #[test]
    fn a_block_image_fills_the_width_a_badge_keeps_its_size_and_native_keeps_today() {
        let c = |cols, rows| Cells { cols, rows };
        let l = |size, attrs, cols, rows, w| layout(size, attrs, (10, 20), cols, rows, w);
        let (fill, native) = (Width::Fill, Width::Native);
        // A 1284×600 diagram at 10×20 px in 200 columns: natively 128×30, filled 200 wide.
        assert_eq!(l((1284, 600), (None, None), 200, 40, native), c(128, 30));
        assert_eq!(l((1284, 600), (None, None), 200, 40, fill), c(171, 40));
        // 1284×600 into 200 columns would be 47 rows: the 40-row cap binds first. A tighter
        // cap narrows it further, aspect kept.
        assert_eq!(l((1284, 600), (None, None), 200, 20, fill), c(86, 20));
        // `width` attributes set the aspect; fill still takes the width.
        assert_eq!(l((3000, 3000), (Some(1284), Some(600)), 120, 40, fill), c(120, 28));
        // A badge — one row at its own size — keeps its size in either mode.
        assert_eq!(l((90, 20), (None, None), 200, 40, fill), c(9, 1));
        assert_eq!(l((90, 20), (None, None), 200, 40, native), c(9, 1));
        // Row cap 0 is none: the protocol's own limit.
        assert_eq!(row_cap(0), graphics::MAX_SPAN as u16);
        assert_eq!(row_cap(40), 40);
        assert_eq!(l((200, 4000), (None, None), 100, row_cap(0), fill), c(30, 297));
        assert_eq!(l((200, 4000), (None, None), 100, row_cap(0), native), c(20, 200));
        assert_eq!(
            l((200, 4000), (None, None), 100, 40, native),
            c(4, 40),
            "the cap holds natively"
        );
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
        let file_page = "https://github.com/o/r/raw/diagrams/7/abc/c-light-1f.svg";
        assert_eq!(
            resolve(file_page, gh).as_deref(),
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
        assert_eq!(resolve(file_page, Base::default()).as_deref(), Some(file_page));
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

    const CELL: (u16, u16) = (10, 20);

    fn at(store: &mut Store, painted: &[(&str, Cells)], now: Instant) -> Placed {
        let painted: Vec<_> = painted.iter().map(|(u, c)| ((*u).to_string(), *c)).collect();
        store.place(&painted, CELL, now)
    }

    /// The bytes of `bytes` that are image payload: every chunk's data after its `;`.
    fn payload_bytes(bytes: &[u8]) -> usize {
        String::from_utf8_lossy(bytes)
            .split("\x1b\\")
            .filter_map(|c| c.split_once(';').map(|(_, data)| data.len()))
            .sum()
    }

    #[test]
    fn the_store_sends_a_picture_once_and_moves_only_its_placement() {
        let now = Instant::now();
        let mut store = Store::default();
        let cells = Cells { cols: 9, rows: 1 };
        store.mark_requested("a");
        store.mark_requested("b");
        assert!(store.pending() && store.size("a").is_none());
        store.land("a".into(), Some(ready((90, 20), 5400)));
        store.land("b".into(), None);
        assert!(!store.pending());
        assert_eq!(store.size("a"), Some((90, 20)));
        assert_eq!(store.size("b"), None, "a failure stays its alt link");
        assert_eq!(store.placed_id("a"), None, "landed but not on the terminal");

        let placed = at(&mut store, &[("a", cells), ("b", cells)], now);
        assert!(placed.sent);
        let seq = String::from_utf8(placed.bytes).unwrap();
        assert_eq!(
            seq,
            "\x1b_Ga=t,f=100,i=6291456,q=2,m=0;QUJD\x1b\\\x1b_Ga=p,U=1,i=6291456,p=1,c=9,r=1,q=2\x1b\\",
            "transmitted once, then one virtual placement"
        );
        let id = store.placed_id("a").unwrap();
        let again = at(&mut store, &[("a", cells)], now);
        assert!(again.bytes.is_empty() && !again.sent, "nothing to say twice");
        // A new footprint moves the placement only: no pixels.
        let wide = Cells { cols: 18, rows: 2 };
        let moved = at(&mut store, &[("a", wide)], now);
        assert_eq!(
            String::from_utf8(moved.bytes).unwrap(),
            format!("\x1b_Ga=p,U=1,i={id},p=1,c=18,r=2,q=2\x1b\\")
        );
        assert!(!moved.sent, "the frame already paints it");
        assert_eq!(store.placed_id("a"), Some(id), "one id for every size");
        assert_eq!(store.deletions(), graphics::delete(id));
        assert!(store.deletions().is_empty(), "deleted once");
        assert!(at(&mut store, &[("a", cells)], now).sent, "painted again: sent again");
    }

    #[test]
    fn a_resize_burst_writes_no_image_payload() {
        // K images on screen, then N frames each at a new width: only placements go out.
        let (k, n) = (6, 50);
        let now = Instant::now();
        let mut store = Store::default();
        let big = Arc::<str>::from("A".repeat(400_000));
        for i in 0..k {
            let p = Prepared { payload: big.clone(), ..ready((1284, 600), 1284 * 600) };
            store.land(i.to_string(), Some(p));
        }
        let frame = |cols: u16| -> Vec<(String, Cells)> {
            (0..k).map(|i| (i.to_string(), Cells { cols, rows: cols / 2 })).collect()
        };
        let first = store.place(&frame(80), CELL, now);
        assert_eq!(payload_bytes(&first.bytes), k * 400_000, "each picture once");
        let mut burst = Vec::new();
        for i in 0..n {
            burst.extend(store.place(&frame(40 + i as u16), CELL, now).bytes);
        }
        assert_eq!(payload_bytes(&burst), 0, "no pixels during the burst");
        assert!(burst.len() < k * n * 64, "placements only: {} bytes", burst.len());
    }

    #[test]
    fn past_the_budget_the_least_recently_painted_image_is_deleted_first() {
        let now = Instant::now();
        let mut store = Store::default();
        let cells = Cells { cols: 4, rows: 2 };
        let big = MAX_PIXELS / 2;
        for url in ["a", "b", "c"] {
            store.land(url.into(), Some(ready((40, 40), big)));
        }
        at(&mut store, &[("a", cells)], now);
        at(&mut store, &[("b", cells)], now);
        let a = store.placed_id("a").unwrap();
        // `c` overflows the pixels: `a`, painted longest ago, goes.
        let placed = at(&mut store, &[("c", cells), ("b", cells)], now);
        assert!(placed.sent);
        assert!(placed.bytes.starts_with(&graphics::delete(a)));
        assert_eq!(store.placed_id("a"), None);
        assert!(store.placed_id("b").is_some() && store.placed_id("c").is_some());
        // Everything on screen this frame stays: a third that cannot fit keeps its alt.
        let placed = at(&mut store, &[("a", cells), ("b", cells), ("c", cells)], now);
        assert!(!placed.sent);
        assert_eq!(store.placed_id("a"), None);

        // The count cap holds the same way.
        let mut store = Store::default();
        let painted: Vec<_> = (0..=MAX_PLACED)
            .map(|i| {
                store.land(i.to_string(), Some(ready((10, 10), 100)));
                (i.to_string(), cells)
            })
            .collect();
        for one in &painted {
            store.place(std::slice::from_ref(one), CELL, now);
        }
        assert_eq!(store.held().0, MAX_PLACED);
        assert_eq!(store.placed_id("0"), None, "the oldest made room");
    }

    #[test]
    fn an_svg_that_outgrows_its_raster_is_redrawn_sharp_once_resizes_go_quiet() {
        let svg: Arc<[u8]> = Arc::from(&br##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"><rect width="100" height="50" fill="#123"/></svg>"##[..]);
        let t0 = Instant::now();
        let mut store = Store::default();
        let packed =
            Prepared { raster: (300, 150), svg: Some(svg.clone()), ..ready((100, 50), 45_000) };
        store.land("s".into(), Some(packed));
        // 30×5 cells is 300×100 px: the packed raster covers it, nothing to redraw.
        let small = Cells { cols: 30, rows: 5 };
        let placed = at(&mut store, &[("s", small)], t0);
        assert!(placed.sent && placed.rasters.is_empty());
        assert!(!store.sharp_pending());
        // 80×20 cells is 800×400 px: the block outgrew it. The packed raster stays placed
        // at the new cells, and nothing is asked until the size has held for QUIET.
        let big = Cells { cols: 80, rows: 20 };
        let placed = at(&mut store, &[("s", big)], t0);
        assert!(placed.rasters.is_empty(), "debounced");
        assert_eq!(payload_bytes(&placed.bytes), 0, "only the placement moved");
        assert!(store.sharp_pending(), "the loop keeps waking for it");
        let mid = Cells { cols: 90, rows: 25 };
        assert!(at(&mut store, &[("s", mid)], t0 + QUIET / 2).rasters.is_empty());
        // A new size restarts the wait; once it holds, one ask at the block's pixel size.
        assert!(at(&mut store, &[("s", big)], t0 + QUIET).rasters.is_empty());
        let ask = at(&mut store, &[("s", big)], t0 + QUIET * 2).rasters;
        assert_eq!(ask.len(), 1);
        assert_eq!(ask[0].px, (800, 400), "fitted into 800×400 px, aspect kept");
        assert!(at(&mut store, &[("s", big)], t0 + QUIET * 3).rasters.is_empty(), "asked once");

        // A superseded ask lands to nothing; the latest swaps in, freeing the old raster.
        let id = store.placed_id("s").unwrap();
        store.land_sharp((
            "s".into(),
            ask[0].px,
            ask[0].tag + 99,
            Some((Arc::from("Tk9QRQ=="), 1)),
        ));
        assert!(at(&mut store, &[("s", big)], t0 + QUIET * 3).bytes.is_empty(), "stale tag");
        let drawn = rasterize_svg_to(&svg, ask[0].px).expect("rasterises at the size");
        store.land_sharp(("s".into(), ask[0].px, ask[0].tag, Some(drawn.clone())));
        let swap = at(&mut store, &[("s", big)], t0 + QUIET * 3);
        let seq = String::from_utf8(swap.bytes).unwrap();
        assert!(seq.starts_with(&String::from_utf8(graphics::delete(id)).unwrap()), "old freed");
        assert!(seq.contains(&format!("a=t,f=100,i={id},q=2,")), "same id, new pixels");
        assert!(seq.ends_with(&format!("a=p,U=1,i={id},p=1,c=80,r=20,q=2\x1b\\")));
        assert_eq!(payload_bytes(seq.as_bytes()), drawn.0.len());
        assert!(!store.sharp_pending());
        assert_eq!(store.held().1, drawn.1, "the budget counts the sharp raster");
        // Back to a size the packed raster covers: the sharp one stays — nothing re-sent.
        assert!(at(&mut store, &[("s", small)], t0 + QUIET * 4).rasters.is_empty());
    }

    #[test]
    fn an_svg_redraws_at_the_pixel_size_asked() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"><rect width="100" height="50" fill="#123"/></svg>"##;
        let (payload, pixels) = rasterize_svg_to(svg, (800, 800)).unwrap();
        assert_eq!(pixels, 800 * 400, "fitted, aspect kept");
        assert!(payload.starts_with("iVBORw0KGgo"));
        assert_eq!(sharp_px((100, 50), Cells { cols: 80, rows: 40 }, (10, 20)), (800, 400));
        // Capped for terminal memory.
        let (w, h) = sharp_px((4000, 4000), Cells { cols: 297, rows: 297 }, (40, 80));
        assert!(u64::from(w) * u64::from(h) <= SHARP_MAX_PIXELS + 4000);
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
