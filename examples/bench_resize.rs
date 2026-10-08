//! Bytes reviewr writes to the terminal for inline images across a burst of resizes:
//!
//! `cargo run --release --example bench_resize -- [images] [resizes] [cell-w] [cell-h]`
//!
//! It prepares `images` pictures the way the download worker does (an SVG diagram and a
//! photo-like PNG, alternating), puts them on screen, then replays `resizes` pane resizes,
//! each giving every image a new footprint. "before" re-sends every picture on each resize,
//! as reviewr did when a resize forgot what the terminal held; "after" is today's store, where
//! a resize moves placements only. The sharp SVG re-raster that follows once resizes settle is
//! counted separately: it happens once per image, after the burst.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use herdr_reviewr::images::{self, Cells, Prepared, Store};

fn diagram() -> Vec<u8> {
    let mut svg = String::from(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="1284" height="600"><rect width="1284" height="600" fill="#fafafa"/>"##,
    );
    for i in 0..40 {
        let (x, y) = (20 + (i % 8) * 155, 20 + (i / 8) * 115);
        let _ = write!(
            svg,
            r##"<rect x="{x}" y="{y}" width="140" height="90" rx="8" fill="#{:02x}88cc" stroke="#333"/><path d="M{} {} L{} {}" stroke="#666"/>"##,
            (i * 6) % 256,
            x + 140,
            y + 45,
            x + 155,
            y + 45
        );
    }
    svg.push_str("</svg>");
    svg.into_bytes()
}

fn photo() -> Vec<u8> {
    let img = image::RgbaImage::from_fn(1600, 1000, |x, y| {
        let n = (x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503)) >> 24;
        image::Rgba([(x / 7) as u8 ^ n as u8, (y / 5) as u8, (x + y) as u8 / 3, 255])
    });
    let mut png = Vec::new();
    let _ = image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png);
    png
}

/// The image-payload bytes of a terminal write: every graphics chunk's data.
fn payload(bytes: &[u8]) -> usize {
    String::from_utf8_lossy(bytes)
        .split("\x1b\\")
        .filter_map(|c| c.split_once(';').map(|(_, d)| d.len()))
        .sum()
}

fn main() {
    let mut args = std::env::args().skip(1).map(|a| a.parse::<usize>().unwrap_or(0));
    let k = args.next().filter(|n| *n > 0).unwrap_or(4);
    let n = args.next().filter(|n| *n > 0).unwrap_or(60);
    let cw = args.next().filter(|n| *n > 0).unwrap_or(10) as u16;
    let ch = args.next().filter(|n| *n > 0).unwrap_or(20) as u16;
    let cell = (cw, ch);
    let sources = [images::prepare(&diagram()), images::prepare(&photo())];
    let prepared: Vec<Prepared> = (0..k).map(|i| sources[i % 2].clone().unwrap()).collect();
    let frame = |cols: u16| -> Vec<(String, Cells)> {
        prepared
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let c = images::layout(
                    p.size,
                    (None, None),
                    cell,
                    cols.into(),
                    40,
                    images::Width::Fill,
                );
                (i.to_string(), c)
            })
            .collect()
    };
    let land = || {
        let mut store = Store::default();
        for (i, p) in prepared.iter().enumerate() {
            store.land(i.to_string(), Some(p.clone()));
        }
        store
    };
    let t0 = Instant::now();

    let mut before = land();
    before.place(&frame(120), cell, t0);
    let (mut b_total, mut b_payload) = (0, 0);
    for i in 0..n {
        let _ = before.deletions();
        let out = before.place(&frame(60 + i as u16), cell, t0).bytes;
        b_total += out.len();
        b_payload += payload(&out);
    }

    let mut after = land();
    after.place(&frame(120), cell, t0);
    let (mut a_total, mut a_payload) = (0, 0);
    for i in 0..n {
        let out = after.place(&frame(60 + i as u16), cell, t0).bytes;
        a_total += out.len();
        a_payload += payload(&out);
    }
    // Resizes go quiet: each SVG that outgrew its packed raster is redrawn once.
    let settled = after.place(&frame(60 + n as u16 - 1), cell, t0 + Duration::from_millis(200));
    let started = Instant::now();
    let sharp: usize = settled
        .rasters
        .iter()
        .filter_map(|job| images::rasterize_svg_to(&job.svg, job.px))
        .map(|(p, _)| p.len())
        .sum();
    let raster_time = started.elapsed();
    // Byte counts far inside f64's exact range.
    #[allow(clippy::cast_precision_loss)]
    let mb = |b: usize| b as f64 / 1_000_000.0;
    println!("{k} images, {n} resizes, {}×{} px cells", cell.0, cell.1);
    println!(
        "packed payloads: {:.2} MB each SVG, {:.2} MB each photo",
        mb(sources[0].as_ref().map_or(0, |p| p.payload.len())),
        mb(sources[1].as_ref().map_or(0, |p| p.payload.len()))
    );
    println!("before: {:.2} MB written ({:.2} MB image payload)", mb(b_total), mb(b_payload));
    println!("after:  {:.4} MB written ({} bytes image payload)", mb(a_total), a_payload);
    println!(
        "after the burst settles: {} sharp SVG re-raster(s), {:.2} MB, drawn in {raster_time:?} off the frame loop",
        settled.rasters.len(),
        mb(sharp)
    );
}
