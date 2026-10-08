//! Manual check that inline PR images can show in this terminal — run it inside a herdr pane:
//!
//! `cargo run --example image_check -- [image-url ...]`
//!
//! It sends reviewr's Kitty graphics probe and reads the answer the way reviewr does, prints
//! the verdict and the cell size, then paints each image the way the PR read pane does: packed
//! as a PNG, sized into cells (at most 20 rows, the terminal's width), and drawn through Unicode
//! placeholder cells between `│` markers. Without URLs it paints a built-in shields-style SVG
//! badge (text included, through the system's fonts) and a test gradient. A URL on github.com
//! goes with the `gh` token, as reviewr sends it for a GitHub PR. A working terminal shows each
//! picture in its box; one without the protocol shows blank or odd cells, and reviewr would
//! paint the `⧉ alt` link there instead.

use std::io::Write;
use std::time::{Duration, Instant};

use herdr_reviewr::graphics::{self, Fed, ProbeFilter};
use herdr_reviewr::images;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode, window_size};

const BADGE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="90" height="20">
<linearGradient id="b" x2="0" y2="100%"><stop offset="0" stop-color="#bbb" stop-opacity=".1"/><stop offset="1" stop-opacity=".1"/></linearGradient>
<rect rx="3" width="90" height="20" fill="#555"/><rect rx="3" x="37" width="53" height="20" fill="#4c1"/>
<rect rx="3" width="90" height="20" fill="url(#b)"/>
<g fill="#fff" text-anchor="middle" font-family="Verdana,Geneva,DejaVu Sans,sans-serif" font-size="11">
<text x="19" y="14">build</text><text x="62" y="14">passing</text></g></svg>"##;

fn main() {
    let urls: Vec<String> = std::env::args().skip(1).collect();
    let mut out = std::io::stdout();

    let _ = enable_raw_mode();
    let mut probe = ProbeFilter::default();
    probe.start(Instant::now());
    let _ = out.write_all(graphics::PROBE.as_bytes());
    let _ = out.flush();
    while probe.waiting() {
        probe.expire(Instant::now());
        if event::poll(Duration::from_millis(20)).unwrap_or(false)
            && let Ok(Event::Key(k)) = event::read()
            && let KeyCode::Char(ch) = k.code
        {
            let _: Fed = probe.feed(ch, k.modifiers.contains(KeyModifiers::ALT));
        }
    }
    let _ = disable_raw_mode();
    let ok = probe.answer() == Some(true);
    println!("kitty graphics probe: {}", if ok { "answered OK" } else { "no answer" });

    let (cell, columns) = match window_size() {
        Ok(w) if w.columns > 0 && w.width > 0 => {
            ((w.width / w.columns, w.height / w.rows), usize::from(w.columns))
        }
        _ => (graphics::FALLBACK_CELL, 80),
    };
    println!("cell size: {}×{} px", cell.0, cell.1);

    let mut sources: Vec<(String, Option<Vec<u8>>)> = Vec::new();
    if urls.is_empty() {
        sources.push(("built-in SVG badge".into(), Some(BADGE.as_bytes().to_vec())));
        let gradient = image::RgbaImage::from_fn(320, 120, |x, y| {
            image::Rgba([(x * 255 / 320) as u8, (y * 2) as u8, 200, 255])
        });
        let mut png = Vec::new();
        let _ = image::DynamicImage::ImageRgba8(gradient)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png);
        sources.push(("built-in 320×120 gradient".into(), Some(png)));
    }
    let limits = graphics::Limits { max_bytes: 10_000_000, max_time: 20, https_redirects: true };
    for url in urls {
        let token = graphics::token_host(&url, "github.com").and_then(|h| graphics::gh_token(&h));
        let bytes = graphics::curl(&url, limits, token.as_deref());
        sources.push((url, bytes));
    }

    let mut ids = Vec::new();
    for (n, (name, bytes)) in sources.into_iter().enumerate() {
        let Some(prepared) = bytes.as_deref().and_then(images::prepare) else {
            println!("{name}: download or decode failed — reviewr keeps the alt link");
            continue;
        };
        let room = columns.saturating_sub(4).max(1);
        let cells = images::fit(prepared.size, (None, None), cell, room, images::MAX_ROWS);
        let id = 0x00_60_F0_00 + n as u32;
        ids.push(id);
        let head = format!("a=T,U=1,f=100,i={id},c={},r={},q=2,", cells.cols, cells.rows);
        let _ = out.write_all(&graphics::chunked(&head, &prepared.payload));
        println!(
            "{name}: {}×{} px → {}×{} cells",
            prepared.size.0, prepared.size.1, cells.cols, cells.rows
        );
        let (r, g, b) = graphics::id_rgb(id);
        for row in 0..usize::from(cells.rows) {
            let line: String =
                (0..usize::from(cells.cols)).map(|col| graphics::placeholder(row, col)).collect();
            println!("│\x1b[38;2;{r};{g};{b}m{line}\x1b[39m│");
        }
    }
    let _ = out.flush();
    println!("press enter to clean up");
    let _ = std::io::stdin().read_line(&mut String::new());
    for id in ids {
        let _ = out.write_all(&graphics::delete(id));
    }
    let _ = out.flush();
}
