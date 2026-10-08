//! The pixel-graphics layer shared by author avatars (`crate::avatar`) and inline PR images
//! (`crate::images`): the Kitty graphics protocol with Unicode placeholders.
//!
//! A picture reaches the pane in two halves that never wait on each other. Out of band, the
//! event loop transmits an image under an id with a virtual placement (`U=1`) a number of
//! cells wide and tall. In band, the renderer paints ordinary text cells — U+10EEEE plus a
//! row and a column diacritic, the image id in the foreground colour — that the terminal
//! swaps for that part of the picture. Downloads run on [`Fetcher`] threads (`curl`, size
//! and time caps), and the [`PROBE`] asks the terminal once, through the input stream,
//! whether it speaks the protocol at all. Anything missing keeps the text fallback.

use std::io::{Read, Write};
use std::process::Stdio;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The Kitty Unicode placeholder: a cell the terminal replaces with part of an image.
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// The Kitty placeholder table: diacritic `n` names row or column `n` of a placement.
const DIACRITICS: [char; 297] = [
    '\u{0305}',
    '\u{030D}',
    '\u{030E}',
    '\u{0310}',
    '\u{0312}',
    '\u{033D}',
    '\u{033E}',
    '\u{033F}',
    '\u{0346}',
    '\u{034A}',
    '\u{034B}',
    '\u{034C}',
    '\u{0350}',
    '\u{0351}',
    '\u{0352}',
    '\u{0357}',
    '\u{035B}',
    '\u{0363}',
    '\u{0364}',
    '\u{0365}',
    '\u{0366}',
    '\u{0367}',
    '\u{0368}',
    '\u{0369}',
    '\u{036A}',
    '\u{036B}',
    '\u{036C}',
    '\u{036D}',
    '\u{036E}',
    '\u{036F}',
    '\u{0483}',
    '\u{0484}',
    '\u{0485}',
    '\u{0486}',
    '\u{0487}',
    '\u{0592}',
    '\u{0593}',
    '\u{0594}',
    '\u{0595}',
    '\u{0597}',
    '\u{0598}',
    '\u{0599}',
    '\u{059C}',
    '\u{059D}',
    '\u{059E}',
    '\u{059F}',
    '\u{05A0}',
    '\u{05A1}',
    '\u{05A8}',
    '\u{05A9}',
    '\u{05AB}',
    '\u{05AC}',
    '\u{05AF}',
    '\u{05C4}',
    '\u{0610}',
    '\u{0611}',
    '\u{0612}',
    '\u{0613}',
    '\u{0614}',
    '\u{0615}',
    '\u{0616}',
    '\u{0617}',
    '\u{0657}',
    '\u{0658}',
    '\u{0659}',
    '\u{065A}',
    '\u{065B}',
    '\u{065D}',
    '\u{065E}',
    '\u{06D6}',
    '\u{06D7}',
    '\u{06D8}',
    '\u{06D9}',
    '\u{06DA}',
    '\u{06DB}',
    '\u{06DC}',
    '\u{06DF}',
    '\u{06E0}',
    '\u{06E1}',
    '\u{06E2}',
    '\u{06E4}',
    '\u{06E7}',
    '\u{06E8}',
    '\u{06EB}',
    '\u{06EC}',
    '\u{0730}',
    '\u{0732}',
    '\u{0733}',
    '\u{0735}',
    '\u{0736}',
    '\u{073A}',
    '\u{073D}',
    '\u{073F}',
    '\u{0740}',
    '\u{0741}',
    '\u{0743}',
    '\u{0745}',
    '\u{0747}',
    '\u{0749}',
    '\u{074A}',
    '\u{07EB}',
    '\u{07EC}',
    '\u{07ED}',
    '\u{07EE}',
    '\u{07EF}',
    '\u{07F0}',
    '\u{07F1}',
    '\u{07F3}',
    '\u{0816}',
    '\u{0817}',
    '\u{0818}',
    '\u{0819}',
    '\u{081B}',
    '\u{081C}',
    '\u{081D}',
    '\u{081E}',
    '\u{081F}',
    '\u{0820}',
    '\u{0821}',
    '\u{0822}',
    '\u{0823}',
    '\u{0825}',
    '\u{0826}',
    '\u{0827}',
    '\u{0829}',
    '\u{082A}',
    '\u{082B}',
    '\u{082C}',
    '\u{082D}',
    '\u{0951}',
    '\u{0953}',
    '\u{0954}',
    '\u{0F82}',
    '\u{0F83}',
    '\u{0F86}',
    '\u{0F87}',
    '\u{135D}',
    '\u{135E}',
    '\u{135F}',
    '\u{17DD}',
    '\u{193A}',
    '\u{1A17}',
    '\u{1A75}',
    '\u{1A76}',
    '\u{1A77}',
    '\u{1A78}',
    '\u{1A79}',
    '\u{1A7A}',
    '\u{1A7B}',
    '\u{1A7C}',
    '\u{1B6B}',
    '\u{1B6D}',
    '\u{1B6E}',
    '\u{1B6F}',
    '\u{1B70}',
    '\u{1B71}',
    '\u{1B72}',
    '\u{1B73}',
    '\u{1CD0}',
    '\u{1CD1}',
    '\u{1CD2}',
    '\u{1CDA}',
    '\u{1CDB}',
    '\u{1CE0}',
    '\u{1DC0}',
    '\u{1DC1}',
    '\u{1DC3}',
    '\u{1DC4}',
    '\u{1DC5}',
    '\u{1DC6}',
    '\u{1DC7}',
    '\u{1DC8}',
    '\u{1DC9}',
    '\u{1DCB}',
    '\u{1DCC}',
    '\u{1DD1}',
    '\u{1DD2}',
    '\u{1DD3}',
    '\u{1DD4}',
    '\u{1DD5}',
    '\u{1DD6}',
    '\u{1DD7}',
    '\u{1DD8}',
    '\u{1DD9}',
    '\u{1DDA}',
    '\u{1DDB}',
    '\u{1DDC}',
    '\u{1DDD}',
    '\u{1DDE}',
    '\u{1DDF}',
    '\u{1DE0}',
    '\u{1DE1}',
    '\u{1DE2}',
    '\u{1DE3}',
    '\u{1DE4}',
    '\u{1DE5}',
    '\u{1DE6}',
    '\u{1DFE}',
    '\u{20D0}',
    '\u{20D1}',
    '\u{20D4}',
    '\u{20D5}',
    '\u{20D6}',
    '\u{20D7}',
    '\u{20DB}',
    '\u{20DC}',
    '\u{20E1}',
    '\u{20E7}',
    '\u{20E9}',
    '\u{20F0}',
    '\u{2CEF}',
    '\u{2CF0}',
    '\u{2CF1}',
    '\u{2DE0}',
    '\u{2DE1}',
    '\u{2DE2}',
    '\u{2DE3}',
    '\u{2DE4}',
    '\u{2DE5}',
    '\u{2DE6}',
    '\u{2DE7}',
    '\u{2DE8}',
    '\u{2DE9}',
    '\u{2DEA}',
    '\u{2DEB}',
    '\u{2DEC}',
    '\u{2DED}',
    '\u{2DEE}',
    '\u{2DEF}',
    '\u{2DF0}',
    '\u{2DF1}',
    '\u{2DF2}',
    '\u{2DF3}',
    '\u{2DF4}',
    '\u{2DF5}',
    '\u{2DF6}',
    '\u{2DF7}',
    '\u{2DF8}',
    '\u{2DF9}',
    '\u{2DFA}',
    '\u{2DFB}',
    '\u{2DFC}',
    '\u{2DFD}',
    '\u{2DFE}',
    '\u{2DFF}',
    '\u{A66F}',
    '\u{A67C}',
    '\u{A67D}',
    '\u{A6F0}',
    '\u{A6F1}',
    '\u{A8E0}',
    '\u{A8E1}',
    '\u{A8E2}',
    '\u{A8E3}',
    '\u{A8E4}',
    '\u{A8E5}',
    '\u{A8E6}',
    '\u{A8E7}',
    '\u{A8E8}',
    '\u{A8E9}',
    '\u{A8EA}',
    '\u{A8EB}',
    '\u{A8EC}',
    '\u{A8ED}',
    '\u{A8EE}',
    '\u{A8EF}',
    '\u{A8F0}',
    '\u{A8F1}',
    '\u{AAB0}',
    '\u{AAB2}',
    '\u{AAB3}',
    '\u{AAB7}',
    '\u{AAB8}',
    '\u{AABE}',
    '\u{AABF}',
    '\u{AAC1}',
    '\u{FE20}',
    '\u{FE21}',
    '\u{FE22}',
    '\u{FE23}',
    '\u{FE24}',
    '\u{FE25}',
    '\u{FE26}',
    '\u{10A0F}',
    '\u{10A38}',
    '\u{1D185}',
    '\u{1D186}',
    '\u{1D187}',
    '\u{1D188}',
    '\u{1D189}',
    '\u{1D1AA}',
    '\u{1D1AB}',
    '\u{1D1AC}',
    '\u{1D1AD}',
    '\u{1D242}',
    '\u{1D243}',
    '\u{1D244}',
];

/// The most rows or columns one placement can name: the diacritic table's length.
pub const MAX_SPAN: usize = DIACRITICS.len();

/// The text of placeholder cell (`row`, `col`) of a placement, one display column wide.
/// Indices past the table clamp to its last entry.
#[must_use]
pub fn placeholder(row: usize, col: usize) -> String {
    let mark = |i: usize| DIACRITICS[i.min(MAX_SPAN - 1)];
    format!("{PLACEHOLDER}{}{}", mark(row), mark(col))
}

/// The foreground colour that names image `id` to the terminal: its low 24 bits as RGB.
#[must_use]
pub fn id_rgb(id: u32) -> (u8, u8, u8) {
    ((id >> 16) as u8, (id >> 8) as u8, id as u8)
}

/// The graphics support query: a 1×1 RGB image the terminal validates without storing,
/// answering `ESC _ G i=31 ; OK ESC \` when it speaks the protocol. Sent once, after the
/// first paint, and never waited on: the answer arrives through the input stream.
pub const PROBE: &str = "\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\";

/// How long the probe's answer may take before the terminal counts as not speaking Kitty
/// graphics. Only bounds the swallowing of the reply; nothing waits on it.
pub const PROBE_WINDOW: Duration = Duration::from_millis(1500);

/// The cell size in pixels when the terminal reports none: the terminal scales an image to
/// its placement's cells anyway, so this only sets how sharp it is and how images size.
pub const FALLBACK_CELL: (u16, u16) = (10, 20);

/// `payload` as graphics commands: the control keys `head` (ending in a comma) on the first
/// chunk only, every chunk but the last flagged `m=1`, at most 4096 payload bytes each — the
/// chunking the protocol requires.
#[must_use]
pub fn chunked(head: &str, payload: &str) -> Vec<u8> {
    const CHUNK: usize = 4096;
    let chunks: Vec<&[u8]> =
        if payload.is_empty() { vec![&[][..]] } else { payload.as_bytes().chunks(CHUNK).collect() };
    let mut out = Vec::with_capacity(payload.len() + 64 * chunks.len());
    for (i, chunk) in chunks.iter().enumerate() {
        let more = u8::from(i + 1 < chunks.len());
        out.extend_from_slice(b"\x1b_G");
        if i == 0 {
            out.extend_from_slice(head.as_bytes());
        }
        out.extend_from_slice(format!("m={more};").as_bytes());
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
    }
    out
}

/// The escape sequence that deletes image `id` and frees its data.
#[must_use]
pub fn delete(id: u32) -> Vec<u8> {
    format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\").into_bytes()
}

/// Standard base64 with padding — the protocol's payload encoding.
#[must_use]
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = match chunk.len() {
            3 => u32::from(chunk[0]) << 16 | u32::from(chunk[1]) << 8 | u32::from(chunk[2]),
            2 => u32::from(chunk[0]) << 16 | u32::from(chunk[1]) << 8,
            _ => u32::from(chunk[0]) << 16,
        };
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// What the probe filter made of one key event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fed {
    /// Not part of a probe reply: handle the key as usual.
    Pass,
    /// Swallowed as part of the reply.
    Swallowed,
}

/// The probe's answer, read back out of the key stream. The reply `ESC _ G … ESC \` reaches
/// the input parser as `Alt+_`, its payload's characters, then `Alt+\`; while the probe is
/// out, the filter swallows exactly that run and reads the verdict from it. The run is
/// bounded in length and in time, so a real `Alt+_` costs at most the keys typed inside the
/// window, and only for a reviewer who opted in.
#[derive(Debug, Default)]
pub struct ProbeFilter {
    deadline: Option<Instant>,
    reply: Option<String>,
    answer: Option<bool>,
}

/// A reply longer than this is not the probe's.
const REPLY_MAX: usize = 96;

impl ProbeFilter {
    /// The probe went out at `now`.
    pub fn start(&mut self, now: Instant) {
        self.deadline = Some(now + PROBE_WINDOW);
    }

    /// The verdict: `None` while the probe is out (text fallbacks meanwhile).
    #[must_use]
    pub fn answer(&self) -> Option<bool> {
        self.answer
    }

    /// Settle a probe whose window passed unanswered: no graphics.
    pub fn expire(&mut self, now: Instant) {
        if self.answer.is_none() && self.deadline.is_some_and(|d| now >= d) {
            self.answer = Some(false);
            self.deadline = None;
            self.reply = None;
        }
    }

    /// Whether the probe is still out, so the loop keeps waking to expire it.
    #[must_use]
    pub fn waiting(&self) -> bool {
        self.answer.is_none() && self.deadline.is_some()
    }

    /// Feed one pressed character; `alt` is its Alt modifier.
    pub fn feed(&mut self, ch: char, alt: bool) -> Fed {
        if self.answer.is_some() || self.deadline.is_none() {
            return Fed::Pass;
        }
        match self.reply.as_mut() {
            None if alt && ch == '_' => {
                self.reply = Some(String::new());
                Fed::Swallowed
            }
            None => Fed::Pass,
            Some(reply) if alt && ch == '\\' => {
                let verdict = parse_reply(reply);
                self.reply = None;
                if let Some(ok) = verdict {
                    self.answer = Some(ok);
                    self.deadline = None;
                }
                Fed::Swallowed
            }
            Some(reply) => {
                reply.push(ch);
                if reply.len() > REPLY_MAX {
                    self.reply = None;
                }
                Fed::Swallowed
            }
        }
    }
}

/// The verdict in one graphics reply payload (`Gi=31;OK`): `Some(true)` for the probe's OK,
/// `Some(false)` for its error, `None` for a reply that is not the probe's.
#[must_use]
pub fn parse_reply(payload: &str) -> Option<bool> {
    let rest = payload.strip_prefix('G')?;
    let (keys, message) = rest.split_once(';')?;
    keys.split(',').any(|k| k == "i=31").then(|| message == "OK")
}

/// One download to make: the URL, and the forge host whose token rides along — set only
/// when [`token_host`] allowed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub url: String,
    pub token_host: Option<String>,
}

/// One finished download: the URL and what the worker made of it, `None` when it failed.
pub type Landing<T> = (String, Option<T>);

/// What one worker does with a request: download and decode, `None` on any failure.
pub type Job<T> = Arc<dyn Fn(&Request) -> Option<T> + Send + Sync>;

/// A download worker: a few threads working through requests in request order. Requests
/// never block; a stalled download holds only its own thread. Dropping the fetcher lets the
/// idle threads exit; a busy one finishes its bounded download first.
pub struct Fetcher<T> {
    jobs: mpsc::Sender<Request>,
    done: mpsc::Receiver<Landing<T>>,
}

impl<T> std::fmt::Debug for Fetcher<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fetcher").finish_non_exhaustive()
    }
}

impl<T: Send + 'static> Fetcher<T> {
    /// Spawn `workers` threads over `job`.
    pub fn spawn_with(
        workers: usize,
        job: impl Fn(&Request) -> Option<T> + Send + Sync + 'static,
    ) -> Self {
        let job: Job<T> = Arc::new(job);
        let (jobs, job_rx) = mpsc::channel::<Request>();
        let (done_tx, done) = mpsc::channel();
        let job_rx = Arc::new(Mutex::new(job_rx));
        for _ in 0..workers.max(1) {
            let job_rx = Arc::clone(&job_rx);
            let done_tx = done_tx.clone();
            let job = Arc::clone(&job);
            std::thread::spawn(move || {
                loop {
                    let next = job_rx.lock().ok().and_then(|rx| rx.recv().ok());
                    let Some(request) = next else { return };
                    let out = job(&request);
                    if done_tx.send((request.url, out)).is_err() {
                        return;
                    }
                }
            });
        }
        Self { jobs, done }
    }

    /// Queue `url` with no token; never waits.
    pub fn request(&self, url: &str) {
        self.request_with(Request { url: url.to_string(), token_host: None });
    }

    /// Queue one request; never waits.
    pub fn request_with(&self, request: Request) {
        let _ = self.jobs.send(request);
    }

    /// One finished download, if any; never waits.
    pub fn try_recv(&self) -> Option<Landing<T>> {
        self.done.try_recv().ok()
    }
}

/// The bounds on one `curl` download.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// The most bytes kept; a larger body fails the download.
    pub max_bytes: u64,
    /// The whole transfer's time limit, in seconds.
    pub max_time: u32,
    /// Follow redirects to `https` only (the first request may still be `http`).
    pub https_redirects: bool,
}

/// Download `url` with `curl`: fail on HTTP errors, follow redirects, a hard time limit, a
/// size cap enforced on the bytes read as well as the announced length, nothing but
/// `http(s)`. A `token` goes as an `Authorization` header written to curl's stdin as its
/// config, never on its command line, so `ps` never shows it. curl drops that header on a
/// redirect to another host. `None` on any failure.
#[must_use]
pub fn curl(url: &str, limits: Limits, token: Option<&str>) -> Option<Vec<u8>> {
    let mut cmd = crate::proc::user_command("curl")?;
    cmd.args(["-fsSL", "--max-time", &limits.max_time.to_string(), "--connect-timeout", "4"])
        .args(["--max-filesize", &limits.max_bytes.to_string(), "--proto", "=https,http"]);
    if limits.https_redirects {
        cmd.args(["--proto-redir", "=https"]);
    }
    let token = token.filter(|t| plausible_token(t));
    if token.is_some() {
        cmd.args(["--config", "-"]);
    }
    cmd.arg("--").arg(url);
    cmd.stdin(if token.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    if let Some(token) = token {
        let mut stdin = child.stdin.take()?;
        let wrote = writeln!(stdin, "header = \"Authorization: token {token}\"");
        drop(stdin);
        if wrote.is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    }
    let mut body = Vec::new();
    let read = child.stdout.take()?.take(limits.max_bytes + 1).read_to_end(&mut body);
    if read.is_err() || body.len() as u64 > limits.max_bytes {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    let status = child.wait().ok()?;
    (status.success() && !body.is_empty()).then_some(body)
}

/// Whether `token` is shaped like a forge token — the only text ever written into curl's
/// config, so nothing in it can close the quoted header and add an option.
fn plausible_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 512
        && token.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// The `gh` token for `host`, read with `gh auth token --hostname`. Run on a download
/// worker, never on the frame loop; never logged.
#[must_use]
pub fn gh_token(host: &str) -> Option<String> {
    let out = crate::proc::command("gh")
        .args(["auth", "token", "--hostname", host])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let token = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && plausible_token(&token)).then_some(token)
}

/// The forge host whose token may go with `url`, `None` for no token. Only over `https`,
/// with no credentials in the URL, and only to the forge's own host, exactly — or, for
/// `github.com`, to `raw.githubusercontent.com`, where a private repository's files are
/// served. The answer names the forge (the host `gh auth token` reads), not the URL's host.
#[must_use]
pub fn token_host(url: &str, forge_host: &str) -> Option<String> {
    let host = https_authority(url)?.to_ascii_lowercase();
    let forge = forge_host.to_ascii_lowercase();
    let raw = forge == "github.com" && host == "raw.githubusercontent.com";
    (!forge.is_empty() && (host == forge || raw)).then_some(forge)
}

/// The authority (`host[:port]`) of an `https` URL, `None` for anything else or for one
/// carrying userinfo.
#[must_use]
pub fn https_authority(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    (!authority.is_empty() && !authority.contains('@')).then_some(authority)
}

/// The authority of an `http(s)` URL — the PR page's host, which names the forge.
#[must_use]
pub fn url_authority(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    (!authority.is_empty() && !authority.contains('@')).then_some(authority)
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn a_placeholder_cell_names_its_row_and_column_in_one_display_column() {
        assert_eq!(placeholder(0, 0), "\u{10EEEE}\u{0305}\u{0305}");
        assert_eq!(placeholder(0, 1), "\u{10EEEE}\u{0305}\u{030D}");
        assert_eq!(placeholder(2, 3), "\u{10EEEE}\u{030E}\u{0310}");
        assert_eq!(placeholder(296, 0).chars().nth(1), Some('\u{1D244}'), "the table's last");
        assert_eq!(placeholder(9999, 0), placeholder(296, 0), "clamped, never a panic");
        for (r, c) in [(0, 0), (5, 40), (19, 296)] {
            assert_eq!(placeholder(r, c).width(), 1);
        }
        let mut seen = std::collections::HashSet::new();
        assert!(DIACRITICS.iter().all(|d| seen.insert(*d)), "every index distinct");
    }

    #[test]
    fn chunked_payloads_carry_their_keys_once_and_flag_every_chunk_but_the_last() {
        assert_eq!(chunked("a=t,", ""), b"\x1b_Ga=t,m=0;\x1b\\");
        let payload = "A".repeat(9000);
        let seq = String::from_utf8(chunked("a=T,i=4,", &payload)).unwrap();
        let chunks: Vec<&str> = seq.split("\x1b\\").filter(|c| !c.is_empty()).collect();
        assert_eq!(chunks.len(), 3);
        assert!(chunks[0].starts_with("\x1b_Ga=T,i=4,m=1;"));
        assert!(chunks[1].starts_with("\x1b_Gm=1;"));
        assert!(chunks[2].starts_with("\x1b_Gm=0;"));
    }

    #[test]
    fn a_token_goes_only_to_the_forge_host_over_https() {
        let forge = "github.com";
        let ok = "https://github.com/user-attachments/assets/abc";
        assert_eq!(token_host(ok, forge).as_deref(), Some("github.com"));
        assert_eq!(token_host("https://GitHub.com/x.png", forge).as_deref(), Some("github.com"));
        for url in [
            "http://github.com/user-attachments/assets/abc",
            "https://github.com.evil.example/x.png",
            "https://evil.example/github.com/x.png",
            "https://user:pw@github.com/x.png",
            "https://x@github.com/x.png",
            "https://avatars.githubusercontent.com/u/1",
            "http://raw.githubusercontent.com/o/r/main/a.png",
            "https://raw.githubusercontent.com.evil.example/o/r/main/a.png",
            "https://evil-raw.githubusercontent.com/o/r/main/a.png",
            "https://raw.githubusercontent.co/o/r/main/a.png",
            "https://xraw.githubusercontent.com/o/r/main/a.png",
            "https://raw.githubusercontent.com:8443/o/r/main/a.png",
            "https://u@raw.githubusercontent.com/o/r/main/a.png",
            "https://githubusercontent.com/o/r/main/a.png",
            "https://img.shields.io/badge/a-b-c",
            "github.com/x.png",
            "ftp://github.com/x.png",
        ] {
            assert_eq!(token_host(url, forge), None, "{url}");
        }
        assert_eq!(token_host(ok, ""), None, "no forge host: no token");
        // A private repository's raw files: github.com's token, read for github.com.
        for raw in [
            "https://raw.githubusercontent.com/o/r/main/a.png",
            "https://RAW.githubusercontent.com/o/r/main/a.png",
        ] {
            assert_eq!(token_host(raw, forge).as_deref(), Some("github.com"), "{raw}");
        }
        assert_eq!(
            token_host("https://raw.githubusercontent.com/o/r/main/a.png", "ghe.corp"),
            None,
            "a GHES token never leaves its own host"
        );
        assert_eq!(
            token_host("https://ghe.corp:8443/a.png", "ghe.corp:8443").as_deref(),
            Some("ghe.corp:8443")
        );
        assert_eq!(token_host("https://ghe.corp/a.png", "ghe.corp:8443"), None);
    }

    #[test]
    fn only_a_token_shaped_string_is_ever_written_into_curls_config() {
        assert!(plausible_token("gho_abcDEF123"));
        assert!(plausible_token("github_pat_11AA.x-y"));
        for bad in ["", "a\"b", "a b", "a\nheader = x", "tok\\"] {
            assert!(!plausible_token(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_generic_fetcher_lands_in_request_order_without_blocking() {
        let fetcher: Fetcher<usize> = Fetcher::spawn_with(1, |r: &Request| Some(r.url.len()));
        fetcher.request("ab");
        fetcher.request_with(Request { url: "abcd".into(), token_host: Some("h".into()) });
        let mut got = Vec::new();
        let started = Instant::now();
        while got.len() < 2 && started.elapsed() < Duration::from_secs(5) {
            if let Some(landing) = fetcher.try_recv() {
                got.push(landing);
            }
            std::thread::yield_now();
        }
        assert_eq!(got, vec![("ab".to_string(), Some(2)), ("abcd".to_string(), Some(4))]);
    }
}
