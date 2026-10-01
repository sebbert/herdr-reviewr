//! Manual check that avatars can show in this terminal — run it inside a herdr pane:
//!
//! `cargo run --example avatar_check -- [avatar-url]`
//!
//! It sends reviewr's Kitty graphics probe and reads the answer the way reviewr does
//! (through the input parser, never blocking), prints the verdict and the cell size, then
//! paints one avatar per width/fit combination through Unicode placeholder cells between
//! two `│` markers. With a URL it downloads that image with `curl`; without one it draws
//! a test gradient. A working terminal shows four round pictures; a terminal without the
//! protocol shows blank or odd cells, and reviewr would paint dots there instead.

use std::io::Write;
use std::time::{Duration, Instant};

use herdr_reviewr::avatar::{self, Fed, Fit, ProbeFilter};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode, window_size};

fn main() {
    let url = std::env::args().nth(1);
    let mut out = std::io::stdout();

    let _ = enable_raw_mode();
    let mut probe = ProbeFilter::default();
    probe.start(Instant::now());
    let _ = out.write_all(avatar::PROBE.as_bytes());
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
    let graphics = probe.answer() == Some(true);
    println!("kitty graphics probe: {}", if graphics { "answered OK" } else { "no answer" });

    let cell = match window_size() {
        Ok(w) if w.columns > 0 && w.width > 0 => (w.width / w.columns, w.height / w.rows),
        _ => avatar::FALLBACK_CELL,
    };
    println!("cell size: {}×{} px", cell.0, cell.1);

    let source = url
        .as_deref()
        .and_then(avatar::curl)
        .and_then(|bytes| avatar::decode(&bytes))
        .unwrap_or_else(|| {
            image::RgbaImage::from_fn(64, 64, |x, y| {
                image::Rgba([(x * 4) as u8, (y * 4) as u8, 200, 255])
            })
        });

    for (n, (width, fit)) in [(1, Fit::Height), (1, Fit::Width), (2, Fit::Height), (2, Fit::Width)]
        .into_iter()
        .enumerate()
    {
        let id = 0x00_52_F0 + n as u32;
        let g = avatar::geometry(cell, width, fit);
        let _ = out.write_all(&avatar::transmit(id, g.cols, &avatar::paint(&source, &g)));
        let (r, gr, b) = avatar::id_rgb(id);
        let cells: String = (0..usize::from(g.cols)).map(avatar::placeholder_cell).collect();
        println!(
            "width {width}, fit {:<6} │\x1b[38;2;{r};{gr};{b}m{cells}\x1b[39m│  ({} cells)",
            fit.as_str(),
            g.cols
        );
    }
    let _ = out.flush();
    println!("press enter to clean up");
    let _ = std::io::stdin().read_line(&mut String::new());
    for n in 0..4 {
        let _ = out.write_all(&avatar::delete(0x00_52_F0 + n));
    }
    let _ = out.flush();
}
